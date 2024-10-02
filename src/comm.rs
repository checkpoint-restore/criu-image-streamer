use anyhow::Result;
use image_streamer::{
    capture::capture,
    criu_connection::CriuListener,
    extract::serve,
    unix_pipe::UnixPipe,
    util::{self, create_dir_all},
    prnt,
};
use libc::dup;
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use nix::unistd::{close, pipe};
use std::{
    fs::File,
    io::{self, BufWriter, Read, Write, Seek, SeekFrom},
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
}

#[derive(StructOpt, PartialEq, Debug)]
enum Operation {
    /// Capture a CRIU image
    Capture,

    /// Serve a captured CRIU image to CRIU
    Serve,
}

fn spawn_capture_handles(
    dir_path: PathBuf,
    num_pipes: usize,
    r_fds: Vec<RawFd>
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let r_fd = r_fds[i].clone();
            let path = dir_path.clone();
            thread::spawn(move || {
                let output_file_path = path.join(&format!("img-{}.lz4", i));
                let mut output_file = File::create(&output_file_path)
                                    .expect("Unable to create output file path");
                let mut total_bytes_read: u64 = 0;
                let size_placeholder = [0u8; 8];
                output_file.write_all(&size_placeholder)
                    .expect("Could not write size placeholder");
                let mut buf_writer = BufWriter::new(output_file);
                let mut encoder = FrameEncoder::new(&mut buf_writer);
                let mut input_file = unsafe { File::from_raw_fd(r_fd) };
                let mut buffer = [0; 1048576];
                loop {
                    match input_file.read(&mut buffer) {
                        Ok(0) => {
                            let _ = close(r_fd);
                            let _ = encoder.finish();
                            // prepend uncompressed size
                            let mut output_file = buf_writer.into_inner()
                                .expect("Could not get file back from buf writer");
                            output_file.seek(SeekFrom::Start(0))
                                .expect("Could not seek to start");
                            output_file.write_all(&total_bytes_read.to_le_bytes())
                                .expect("Could not write total uncompressed size");
                            output_file.flush().expect("Failed to flush BufWriter");

                            return;
                        }
                        Ok(bytes_read) => {
                            total_bytes_read += bytes_read as u64;
                            encoder.write_all(&buffer[..bytes_read])
                                            .expect("Unable to write all bytes");
                            encoder.flush().expect("Failed to flush encoder");
                        },
                        Err(e) => {
                            if e.kind() != io::ErrorKind::Interrupted {
                                prnt!(&format!("err={}",e));
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
    w_fds: Vec<RawFd>
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let w_fd = w_fds[i].clone();
            let path = dir_path.clone();
            let (tx, rx) = mpsc::channel();
            let h = thread::spawn(move || {
                let input_file_path = path.join(&format!("img-{}.lz4", i));
                let mut input_file = File::open(&input_file_path)
                                        .expect("Unable to open input file path");
                // get prepended uncompressed size
                let mut size_buf = [0u8; 8];
                input_file.read_exact(&mut size_buf).expect("Could not read decompressed size");
                let bytes_to_read = u64::from_le_bytes(size_buf);
                prnt!(&format!("bytes to read = {}", bytes_to_read));
                let mut output_file = unsafe { File::from_raw_fd(w_fd) };
                let mut decoder = FrameDecoder::new(input_file);
                let mut buffer = [0; 1048576]; //524288];
                let mut total_bytes_read = 0;
                tx.send(format!("thread {} ready to read {} bytes",i.clone(),bytes_to_read)).unwrap();
                loop {
                    match decoder.read(&mut buffer) {
                        Ok(0) => {
                            if total_bytes_read == bytes_to_read {
                                return;
                            }
                        }
                        Ok(bytes_read) => {
                            total_bytes_read += bytes_read as u64;
                            //prnt!(&format!("read {} bytes, total bytes read {}", bytes_read, total_bytes_read));
                            let _ = output_file.write_all(&buffer[..bytes_read])
                                        .expect("could not write all bytes");
                            //prnt!(&format!("wrote {} bytes", bytes_read));
                        },
                        Err(e) => {
                            if e.kind() != io::ErrorKind::Interrupted {
                                prnt!(&format!("err={}",e));
                                return;
                            }
                        }
                    }
                }
            });
        match rx.recv() {
            Ok(message) => prnt!(&format!("{} Received: {}",i,  message)),
            Err(e) => prnt!(&format!("{} Failed to receive message: {}",i, e)),
        };
            h

        })
        .collect()
}

fn join_handles(handles: Vec<thread::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.join().unwrap();
    }
}

fn do_capture(dir_path: &Path, num_pipes: usize) -> Result<()> {
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

    let gpu_listener = CriuListener::bind(dir_path, "gpu-capture.sock")?;
    let criu_listener = CriuListener::bind(dir_path, "streamer-capture.sock")?;
    let ced_listener = CriuListener::bind(dir_path, "ced-capture.sock")?;
    eprintln!("r");
    prnt!("all listeners done, ready");

    let handle = thread::spawn(move || {
        let _res = capture(shard_pipes, gpu_listener, criu_listener, ced_listener);
        prnt!("closed w_fds");
    });
    prnt!("spawned capture, sleeping for 10ms");
    thread::sleep(Duration::from_millis(10));

    prnt!("done sleeping, spawning capture handles");
    let handles = spawn_capture_handles(dir_path.to_path_buf(), num_pipes, r_fds);
    prnt!("done spawning capture handles, joining handles");
    let _ = handle.join().unwrap();
    join_handles(handles);
    prnt!("done joining handles, returning");

    Ok(())
}

fn do_serve(dir_path: &Path, num_pipes: usize) -> Result<()> {
    prnt!("entered do_serve");
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut w_fds: Vec<RawFd> = Vec::new();
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(r_fd) };
        close(r_fd).expect("Failed to close original write file descriptor");

        w_fds.push(w_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }

    let handles = spawn_serve_handles(dir_path.to_path_buf(), num_pipes, w_fds);
    let ced_listener = CriuListener::bind(dir_path, "ced-serve.sock")?;
    let gpu_listener = CriuListener::bind(dir_path, "gpu-serve.sock")?;
    let criu_listener = CriuListener::bind(dir_path, "streamer-serve.sock")?;
    let ready_path = dir_path.join("ready");
    prnt!(&format!("ready path = {:?}", ready_path));
    eprintln!("r");
    prnt!("done listener setup, ready");

    let handle = thread::spawn(move || {
        let _res = serve(shard_pipes, &ready_path, ced_listener, gpu_listener, criu_listener);
    });
    thread::sleep(Duration::from_millis(200));
    prnt!("spawned serve, slept 50ms");
    join_handles(handles);
    prnt!("done joining lz4 handles, joining serve handle");
    //let _ = handle.join().unwrap();
    match handle.join() {
        Ok(_) => println!("Thread completed successfully"),
        Err(e) => println!("Thread panicked: {:?}", e),
    }

    prnt!("done joining handles, returning");

    Ok(())
}

fn do_main() -> Result<()> {
    let opts: Opts = Opts::from_args();

    let dir_path_string = &opts.dir;
    let dir_path = Path::new(&dir_path_string);
    //let ready_path = Path::new(
    let num_pipes = opts.num_pipes;

    match opts.operation {
        Operation::Capture => do_capture(dir_path, num_pipes),
        Operation::Serve => do_serve(dir_path, num_pipes),
    }?;
    Ok(())
}

fn main() {
    if let Err(e) = do_main() {
        eprintln!("criu-image-streamer Error: {:#}", e);
    }
}
