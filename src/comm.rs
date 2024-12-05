use anyhow::Result;
use aws_sdk_s3::{
    Client,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use crossbeam_utils::Backoff;
use image_streamer::{
    capture::capture,
    endpoint_connection::EndpointListener,
    extract::{extract, serve},
    unix_pipe::UnixPipe,
    util::{self, create_dir_all},
    prnt,
};
use libc::dup;
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use nix::unistd::{close, pipe};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::io::{FromRawFd, RawFd},
    path::{Path, PathBuf},
    thread,
};
use std::{
    io::Cursor,
    sync::mpsc
};
use structopt::{clap::AppSettings, StructOpt};
use tokio::runtime::Runtime;

#[derive(StructOpt, PartialEq, Debug)]
#[structopt(about,
    // When showing --help, we want to keep the order of arguments defined
    // in the `Opts` struct, as opposed to the default alphabetical order.
    global_setting(AppSettings::DeriveDisplayOrder),
    // help subcommand is not useful, disable it.
    global_setting(AppSettings::DisableHelpSubcommand),
    // subcommand version is not useful, disable it.
    global_setting(AppSettings::VersionlessSubcommands),
)]
struct Opts {
    /// Bucket name in AWS S3 where lz4 images will be uploaded to / downloaded from.
    #[structopt(short = "B", long)]
    bucket: Option<String>,
    /// Images directory where the CRIU UNIX socket is created during streaming operations.
    // The short option -D mimics CRIU's short option for its --dir argument.
    #[structopt(short = "D", long)]
    dir: PathBuf,
    #[structopt(subcommand)]
    operation: Operation,
    #[structopt(short, long)]
    num_pipes: usize,
    /// Indicates whether gpu-controller will be used for the capture operation
    #[structopt(long)]
    gpu: bool,
}

#[derive(StructOpt, PartialEq, Debug)]
enum Operation {
    /// Capture a cedana image
    Capture,

    /// Serve a captured cedana image to cedana, criu, cedana-image-streamer
    Serve,

    /// Extract a captured cedana image to the specified dir
    Extract,
}

fn spawn_capture_handles_local(
    dir_path: PathBuf,
    num_pipes: usize,
    r_fds: Vec<RawFd>,
    use_gpu: bool,
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let r_fd = r_fds[i].clone();
            let path = dir_path.clone();
            thread::spawn(move || {
                let prefix = if use_gpu { "img-w-gpu-" } else { "img-" };
                let filename = &format!("{}{}.lz4", prefix, i);
                let output_file_path = path.join(filename);
                let output_file = File::create(&output_file_path)
                                    .expect("Unable to create output file path");
                let ready_path = path.join("ckpt");
                prnt!("ready_path = {:?}", ready_path);
                let mut encoder = FrameEncoder::new(output_file);
                let mut input_file = unsafe { File::from_raw_fd(r_fd) };
                let mut buffer = vec![0; 1048576];
                let backoff = Backoff::new();
                loop {
                    match input_file.read(&mut buffer) {
                        Ok(0) => {
                            if ready_path.exists() {
                                let _ = close(r_fd);
                                let _ = encoder.finish();
                                return;
                            }
                            prnt!("ready path {:?} does not exist", ready_path);
                            backoff.snooze();
                        }
                        Ok(bytes_read) => {
                            encoder.write_all(&buffer[..bytes_read])
                                            .expect("Unable to write all bytes");
                            encoder.flush().expect("Failed to flush encoder");
                            backoff.reset();
                        },
                        Err(e) => {
                            if e.kind() != io::ErrorKind::Interrupted {
                                prnt!("err={}",e);
                                return;
                            }
                        }
                    }
                }
            })
        })
        .collect()
}

fn create_s3_client() -> Client {
    let config = Runtime::new()
        .unwrap()
        .block_on(aws_config::load_from_env());
    Client::new(&config)
}

async fn initiate_multipart_upload(client: &Client, bucket: &str, key: &str) -> String {
    client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .expect("Failed to initiate multipart upload")
        .upload_id
        .expect("Upload ID not provided")
}

async fn upload_part(
    client: &Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    part_number: i32,
    data: Vec<u8>,
) -> CompletedPart {
    let part = client
        .upload_part()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .part_number(part_number)
        .body(ByteStream::from(data))
        .send()
        .await
        .expect("Failed to upload part");

    CompletedPart::builder()
        .part_number(part_number)
        .e_tag(part.e_tag().unwrap().to_string())
        .build()
}

async fn complete_multipart_upload(
    client: &Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    completed_parts: Vec<CompletedPart>,
) {
    let completed_mpu = CompletedMultipartUpload::builder()
        .set_parts(Some(completed_parts))
        .build();

    client
        .complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .multipart_upload(completed_mpu)
        .send()
        .await
        .expect("Failed to complete multipart upload");
}

fn spawn_capture_handles_remote(
    dir_path: PathBuf,
    bucket: String,
    num_pipes: usize,
    r_fds: Vec<RawFd>,
    use_gpu: bool,
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let bucket_name = bucket.clone();
            let path = dir_path.clone();
            let r_fd = r_fds[i].clone();
            let client = create_s3_client();

            thread::spawn(move || {
                let prefix = if use_gpu { "img-w-gpu-" } else { "img-" };
                let key = format!("{}{}.lz4", prefix, i);
                prnt!("key = {}",key);
                let runtime = Runtime::new().expect("Failed to create Tokio runtime");
                let upload_id = runtime.block_on(async {
                    initiate_multipart_upload(&client, &bucket_name, &key).await
                });
                let ready_path = path.join("ckpt");
                let mut completed_parts = Vec::new();
                let mut part_number = 1;
                let mut encoder = FrameEncoder::new(Vec::new());
                let mut input_file = unsafe { File::from_raw_fd(r_fd) };
                let mut buffer = vec![0; 5242880];
                let backoff = Backoff::new();
                loop {
                    match input_file.read(&mut buffer) {
                        Ok(0) => {
                            if ready_path.exists() {
                                let _ = close(r_fd);
                                prnt!("encoder.get_ref().len() before last drain [{}]",encoder.get_ref().len());
                                encoder.flush().expect("Failed to flush encoder");
                                prnt!("encoder.get_ref().len() after flush [{}]",encoder.get_ref().len());
                                let compressed_data = encoder.get_mut().drain(..).collect();
                                prnt!("encoder.get_ref().len() after last drain [{}]",encoder.get_ref().len());
                                if let Ok(_) = encoder.finish() {
                                    prnt!("encoder.finish returned ok");
                                    let completed_part = runtime.block_on(async {
                                        upload_part(&client, &bucket_name, &key, &upload_id, part_number, compressed_data).await
                                    });
                                    completed_parts.push(completed_part);
                                } else {
                                    prnt!("encoder.finish returned err");
                                }
                                runtime.block_on(async {
                                    complete_multipart_upload(&client, &bucket_name, &key, &upload_id, completed_parts.clone()).await;
                                });
                                return;
                            }
                            prnt!("ready path {:?} does not exist", ready_path);
                            backoff.snooze();
                            return;
                        }
                        Ok(bytes_read) => {
                            encoder.write_all(&buffer[..bytes_read]).expect("Unable to write bytes");
                            encoder.flush().expect("Failed to flush encoder");
                            if encoder.get_ref().len() >= 5242880 { // minimum allowed size for S3
                                let compressed_data = encoder.get_mut().drain(..).collect();
                                let completed_part = runtime.block_on(async {
                                    upload_part(&client, &bucket_name, &key, &upload_id, part_number, compressed_data).await
                                });
                                completed_parts.push(completed_part);
                                part_number += 1;
                            }
                            backoff.reset();
                        }
                        Err(e) => {
                            if e.kind() != std::io::ErrorKind::Interrupted {
                                eprintln!("Error reading from fd {}: {}", r_fd, e);
                                return;
                            }
                        }
                    }
                }
            })
        })
        .collect()
}

fn spawn_serve_handles_local(
    dir_path: PathBuf,
    num_pipes: usize,
    w_fds: Vec<RawFd>,
    input_prefix: &str,
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let w_fd = w_fds[i].clone();
            let path = dir_path.clone();
            let (tx, rx) = mpsc::channel();
            let prefix = input_prefix.to_string();
            let handle = thread::spawn(move || {
                let input_file_path = path.join(&format!("{}{}.lz4", &prefix, i));
                prnt!("input_file_path = {:?}", input_file_path);
                let input_file = File::open(&input_file_path)
                                        .expect("Unable to open input file path");
                let mut output_file = unsafe { File::from_raw_fd(w_fd) };
                let mut decoder = FrameDecoder::new(input_file);
                let mut buffer = vec![0; 1048576];
                tx.send(format!("thread {} ready to read", i.clone())).unwrap();
                loop {
                    match decoder.read(&mut buffer) {
                        Ok(0) => {
                            return;
                        }
                        Ok(bytes_read) => {
                            let _ = output_file.write_all(&buffer[..bytes_read])
                                        .expect("could not write all bytes");
                        },
                        Err(e) => {
                            if e.kind() != io::ErrorKind::Interrupted {
                                prnt!("err={}",e);
                                return;
                            }
                        }
                    }
                }
            });
            match rx.recv() {
                Ok(message) => { prnt!("Received message: {}", message); },
                Err(e) => { prnt!("Failed to receive message: {}", e); },
            };
            handle
        })
        .collect()
}

fn spawn_serve_handles_remote(
    bucket: String,
    num_pipes: usize,
    w_fds: Vec<RawFd>,
    input_prefix: &str,
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let bucket_name = bucket.clone();
            let w_fd = w_fds[i].clone();
            let key = format!("{}{}.lz4", &input_prefix, i);
            let client = create_s3_client();
            let (tx, rx) = mpsc::channel();
            let handle = thread::spawn(move || {
                let mut output_file = unsafe { File::from_raw_fd(w_fd) };
                let mut buffer = vec![0; 1048576];
                let runtime = Runtime::new().expect("Failed to create Tokio runtime");
                runtime.block_on(async {
                    let body = client
                                .get_object()
                                .bucket(&bucket_name)
                                .key(&key)
                                .send()
                                .await
                                .expect("can't find key")
                                .body
                                .collect()
                                .await
                                .expect("can't find body");
                    let bytes = body.into_bytes();
                    let cursor = Cursor::new(bytes);
                    let mut decoder = FrameDecoder::new(cursor);
                    tx.send(format!("thread {} ready to read", i.clone())).unwrap();
                    loop {
                        match decoder.read(&mut buffer) {
                            Ok(0) => {
                                return
                            },
                            Ok(bytes_read) => {
                                let _ = output_file.write_all(&buffer[..bytes_read])
                                            .expect("could not write all bytes");
                            },
                            Err(e) => {
                                if e.kind() != io::ErrorKind::Interrupted {
                                    prnt!("err={}",e);
                                    return;
                                }
                            }
                        }
                    }
                });
            });
            match rx.recv() {
                Ok(message) => { prnt!("Received message: {}", message); },
                Err(e) => { prnt!("Failed to receive message: {}", e); },
            };
            handle
        })
        .collect()
}

fn join_handles(handles: Vec<thread::JoinHandle<()>>) {
    for handle in handles {
        match handle.join() {
            Ok(_) => { prnt!("Shard thread completed successfully"); },
            Err(e) => { prnt!("Shard thread panicked: {:?}", e); },
        }
    }
}

fn do_capture(dir_path: &Path, num_pipes: usize, use_gpu: bool, bucket: Option<String>) -> Result<()> {
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut r_fds: Vec<RawFd> = Vec::new();
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(w_fd) };
        close(w_fd).expect("Failed to close original write file descriptor");

        r_fds.push(r_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }

    let _ret = create_dir_all(dir_path);

    let gpu_listener;
    if use_gpu {
        gpu_listener = Some(EndpointListener::bind(dir_path, "gpu-capture.sock")?);
    } else {
        gpu_listener = None;
    }
    let criu_listener = EndpointListener::bind(dir_path, "streamer-capture.sock")?;
    let ced_listener = EndpointListener::bind(dir_path, "ced-capture.sock")?;
    eprintln!("r");

    let handle = thread::spawn(move || {
        let _res = capture(shard_pipes, gpu_listener, criu_listener, ced_listener);
    });

    let handles = match bucket {
        Some(s) => spawn_capture_handles_remote(dir_path.to_path_buf(), s, num_pipes, r_fds, use_gpu),
        None => spawn_capture_handles_local(dir_path.to_path_buf(), num_pipes, r_fds, use_gpu),
    };

    match handle.join() {
        Ok(_) => { prnt!("Capture thread completed successfully"); },
        Err(e) => { prnt!("Capture thread panicked: {:?}", e); },
    }
    join_handles(handles);

    Ok(())
}

fn find_prefix_in_s3_bucket(bucket: &str, client: &Client) -> String {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let objects = client
            .list_objects_v2()
            .bucket(bucket)
            .send()
            .await
            .expect("Failed to get objects in bucket");
        let mut prefixes = HashSet::new();
        if let Some(contents) = objects.contents {
            for object in contents {
                if let Some(key) = object.key {
                    if key.starts_with("img-") {
                        if key.starts_with("img-w-gpu-") {
                            prefixes.insert("img-w-gpu-".to_string());
                        } else {
                            prefixes.insert("img-".to_string());
                        }
                    }
                }
            }
        }

        match prefixes.len() {
            1 => prefixes.into_iter().next().expect("can't get prefix"), // 1 prefix found
            _ => "".to_string(), // either no prefixes or both found -> error
        }
    })
}

fn find_prefix_in_directory(directory: &Path) -> String {
    let entries = fs::read_dir(directory).unwrap();
    let mut prefixes = HashSet::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let file_name = entry.file_name().into_string().unwrap();
                if file_name.starts_with("img-") {
                    if file_name.starts_with("img-w-gpu-") {
                        prefixes.insert("img-w-gpu-".to_string());
                    } else {
                        prefixes.insert("img-".to_string());
                    }
                }
            }
            Err(_) => continue,
        }
    }

    match prefixes.len() {
        1 => prefixes.into_iter().next().expect("can't get prefix"), // 1 prefix found
        _ => "".to_string(), // either no prefixes or both found -> error
    }
}

fn do_serve(dir_path: &Path, num_pipes: usize, file_prefix: String, bucket: Option<String>) -> Result<()> {
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut w_fds: Vec<RawFd> = Vec::new();
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(r_fd) };
        close(r_fd).expect("Failed to close original read file descriptor");

        w_fds.push(w_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }
    
    let path = dir_path.to_path_buf();
    let ced_listener = EndpointListener::bind(dir_path, "ced-serve.sock")?;
    let gpu_listener;
    if &file_prefix == "img-w-gpu-" {
        gpu_listener = Some(EndpointListener::bind(dir_path, "gpu-serve.sock")?);
    } else {
        gpu_listener = None;
    }
    let criu_listener = EndpointListener::bind(dir_path, "streamer-serve.sock")?;
    eprintln!("r");

    let handle = thread::spawn(move || {
        let _res = serve(shard_pipes, ced_listener, gpu_listener, criu_listener);
    });
    let handles = match bucket {
        Some(ref s) => { 
            prnt!("direct remoting");
            spawn_serve_handles_remote(s.to_string(), num_pipes, w_fds.clone(), &file_prefix)
        },
        None => {
            prnt!("not remoting");
            spawn_serve_handles_local(path, num_pipes, w_fds, &file_prefix)
        },
    };
    prnt!("finished making handles");

    match handle.join() {
        Ok(_) => prnt!("Serve thread completed successfully"),
        Err(e) => prnt!("Serve thread panicked: {:?}", e),
    }
    join_handles(handles);

    Ok(())
}

fn do_extract(dir_path: &Path, num_pipes: usize, file_prefix: String, bucket: Option<String>) -> Result<()> {
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut w_fds: Vec<RawFd> = Vec::new();
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(r_fd) };
        close(r_fd).expect("Failed to close original write file descriptor");

        w_fds.push(w_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }

    let _ret = create_dir_all(dir_path);

    let dir_path_buf = dir_path.to_path_buf();
    let handle = thread::spawn(move || {
        let _res = extract(&dir_path_buf, shard_pipes);
    });

    let handles = match bucket {
        Some(ref s) => { 
            prnt!("direct remoting");
            spawn_serve_handles_remote(s.to_string(), num_pipes, w_fds.clone(), &file_prefix)
        },
        None => {
            prnt!("not remoting");
            spawn_serve_handles_local(dir_path.to_path_buf(), num_pipes, w_fds.clone(), &file_prefix)
        },
    };
    prnt!("finished making handles");
    
    join_handles(handles);

    match handle.join() {
        Ok(_) => prnt!("Extract thread completed successfully"),
        Err(e) => prnt!("Extract thread panicked: {:?}", e),
    }
    eprintln!("r");

    Ok(())
}

fn check_for_gpu(dir_path: &Path, bucket: &Option<String>) -> String {
    let file_prefix = match bucket {
        Some(b) => {
            let config = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(aws_config::load_from_env());
            let client = Client::new(&config);
            find_prefix_in_s3_bucket(b, &client)
        },
        None => find_prefix_in_directory(dir_path),
    };
    if file_prefix == "img-w-gpu-" {
        prnt!("checkpoint contains GPU files");
    } else {
        prnt!("checkpoint does not contain GPU files");
    }
    file_prefix
}

fn do_main() -> Result<()> {
    let opts: Opts = Opts::from_args();

    let bucket = opts.bucket;
    let dir_path_string = &opts.dir;
    let dir_path = Path::new(&dir_path_string);
    let num_pipes = opts.num_pipes;
    let gpu = opts.gpu;

    match opts.operation {
        Operation::Capture => do_capture(dir_path, num_pipes, gpu, bucket),
        Operation::Serve => {
            let prefix = check_for_gpu(dir_path, &bucket);
            if prefix == "" {
                return Err(anyhow::anyhow!("both/no file prefix found in {:?}", dir_path))
            }
            do_serve(dir_path, num_pipes, prefix, bucket)
        },
        Operation::Extract => {
            let prefix = check_for_gpu(dir_path, &bucket);
            if prefix == "" {
                return Err(anyhow::anyhow!("both/no file prefix found in {:?}", dir_path))
            }
            do_extract(dir_path, num_pipes, prefix, bucket)
        },
    }?;
    Ok(())
}

fn main() {
    if let Err(e) = do_main() {
        eprintln!("cedana-image-streamer Error: {:#}", e);
    }
}
