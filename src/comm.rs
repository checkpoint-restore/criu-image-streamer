use anyhow::Result;
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
    time::Duration,
};
use std::sync::mpsc;
use structopt::{clap::AppSettings, StructOpt};

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

fn spawn_capture_handles(
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

fn spawn_serve_handles(
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

fn join_handles(handles: Vec<thread::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.join().expect("failed to join lz4 handle");
        prnt!("joined lz4 handle");
    }
}

fn do_capture(dir_path: &Path, num_pipes: usize, use_gpu: bool) -> Result<()> {
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
    thread::sleep(Duration::from_millis(10));

    let handles = spawn_capture_handles(dir_path.to_path_buf(), num_pipes, r_fds, use_gpu);
    match handle.join() {
        Ok(_) => prnt!("Capture thread completed successfully"),
        Err(e) => prnt!("Capture thread panicked: {:?}", e),
    }
    join_handles(handles);

    Ok(())
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

fn do_serve(dir_path: &Path, num_pipes: usize, file_prefix: String) -> Result<()> {
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut w_fds: Vec<RawFd> = Vec::new();
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(r_fd) };
        close(r_fd).expect("Failed to close original write file descriptor");

        w_fds.push(w_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }

    let path = dir_path.to_path_buf();
    let handles = spawn_serve_handles(path, num_pipes, w_fds, &file_prefix);
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
    match handle.join() {
        Ok(_) => prnt!("Serve thread completed successfully"),
        Err(e) => prnt!("Serve thread panicked: {:?}", e),
    }
    join_handles(handles);

    Ok(())
}

fn do_extract(dir_path: &Path, num_pipes: usize, file_prefix: String) -> Result<()> {
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

    let dir_cp = dir_path.to_path_buf();
    let handles = spawn_serve_handles(dir_cp.clone(), num_pipes, w_fds, &file_prefix);
    let handle = thread::spawn(move || {
        let _res = extract(&dir_cp, shard_pipes);
    });
    join_handles(handles);
    match handle.join() {
        Ok(_) => prnt!("Extract thread completed successfully"),
        Err(e) => prnt!("Extract thread panicked: {:?}", e),
    }
    eprintln!("r");

    Ok(())
}

fn check_for_gpu(dir_path: &Path) -> String {
    let file_prefix = find_prefix_in_directory(dir_path);
    if file_prefix == "img-w-gpu-" {
        prnt!("checkpoint contains GPU files");
    } else {
        prnt!("checkpoint does not contain GPU files");
    }
    file_prefix
}

fn do_main() -> Result<()> {
    let opts: Opts = Opts::from_args();

    let dir_path_string = &opts.dir;
    let dir_path = Path::new(&dir_path_string);
    let num_pipes = opts.num_pipes;
    let gpu = opts.gpu;

    match opts.operation {
        Operation::Capture => do_capture(dir_path, num_pipes, gpu),
        Operation::Serve => {
            let prefix = check_for_gpu(dir_path);
            if prefix == "" {
                return Err(anyhow::anyhow!("both/no file prefix found in {:?}", dir_path))
            }
            do_serve(dir_path, num_pipes, prefix)
        },
        Operation::Extract => {
            let prefix = check_for_gpu(dir_path);
            if prefix == "" {
                return Err(anyhow::anyhow!("both/no file prefix found in {:?}", dir_path))
            }
            do_extract(dir_path, num_pipes, prefix)
        },
    }?;
    Ok(())
}

fn main() {
    if let Err(e) = do_main() {
        eprintln!("cedana-image-streamer Error: {:#}", e);
    }
}
