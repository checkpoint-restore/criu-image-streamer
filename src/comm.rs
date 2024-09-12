use anyhow::Result;
use image_streamer::{
    capture::capture,
    criu_connection::CriuListener,
    extract::serve,
    unix_pipe::UnixPipe,
    util::{create_dir_all, prnt},
};
use libc::dup;
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use nix::unistd::{close, pipe};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::io::{FromRawFd, RawFd},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
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
    #[structopt(subcommand)] //TODO
    operation: Operation,
}

#[derive(StructOpt, PartialEq, Debug)]
enum Operation {
    /// Capture a CRIU image
    Capture,

    /// Serve a captured CRIU image to CRIU
    Serve,
}

fn spawn_handles(
    dir_path: PathBuf,
    handle: Arc<Mutex<thread::JoinHandle<()>>>,
    num_pipes: usize,
    r_fds: Vec<RawFd>
) -> Vec<thread::JoinHandle<()>> {
    (0..num_pipes)
        .map(|i| {
            let handle = Arc::clone(&handle);
            let r_fd = r_fds[i].clone();
            let path = dir_path.clone();
            thread::spawn(move || {
                let output_file_path = path.join(&format!("img-{}.lz4", i));
                let output_file = File::create(output_file_path)
                                    .expect("Unable to create output file path");
                let mut encoder = FrameEncoder::new(output_file);
                let mut input_file = unsafe { File::from_raw_fd(r_fd) };
                let mut buffer = [0; 1048576];
                loop {
                    match input_file.read(&mut buffer) {
                        Ok(0) => {
                            let handle = handle.lock().unwrap();
                            if handle.is_finished() {
                                let _ = encoder.finish();
                                return;
                            }
                        }
                        Ok(bytes_read) => encoder.write_all(&buffer[..bytes_read])
                                            .expect("Unable to write all bytes"),
                        Err(e) => {
                            if e.kind() != io::ErrorKind::Interrupted {
                                prnt(&format!("err={}",e));
                                return;// Err(e.into());
                            }
                        }
                    }
                }
            })
        })
        .collect()
}

fn join_handles(handles: Vec<thread::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.join().unwrap();
    }
}

fn do_capture(dir_path: &Path) -> Result<()> {
    let mut shard_pipes: Vec<UnixPipe> = Vec::new();
    let mut r_fds: Vec<RawFd> = Vec::new();
    let num_pipes = 8;
    for _ in 0..num_pipes {
        let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;
        let dup_fd = unsafe { dup(w_fd) };
        close(w_fd).expect("Failed to close original write file descriptor");

        r_fds.push(r_fd);
        shard_pipes.push(unsafe { File::from_raw_fd(dup_fd) });
    }

    let _ret = create_dir_all(dir_path);

    let gpu_listener = CriuListener::bind_for_capture(dir_path, "gpu-capture.sock")?;
    let criu_listener = CriuListener::bind_for_capture(dir_path, "streamer-capture.sock")?;
    let ced_listener = CriuListener::bind_for_capture(dir_path, "ced-capture.sock")?;
    eprintln!("r");

    let handle = Arc::new(Mutex::new(thread::spawn(move || {
        let _res = capture(shard_pipes, gpu_listener, criu_listener, ced_listener);
        prnt("returned from capture");
    })));
    thread::sleep(Duration::from_millis(10));

    let handles = spawn_handles(dir_path.to_path_buf(), handle, num_pipes, r_fds);
    join_handles(handles);

    Ok(())
}

fn do_serve(dir_path: &Path) -> Result<()> {
    let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;

    let ced_listener = CriuListener::bind_for_restore(dir_path, "ced-serve.sock")?;
    let criu_listener = CriuListener::bind_for_restore(dir_path, "streamer-serve.sock")?;
    eprintln!("r");

    let input_file_path = dir_path.join("img.lz4");
    let input_file = File::open(input_file_path)?;
    let mut output_file = unsafe { File::from_raw_fd(w_fd) };
    let mut decoder = FrameDecoder::new(input_file);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    thread::spawn(move || {
        let _ = output_file.write_all(&buf);
    });

    let dup_fd = unsafe { dup(r_fd) };
    close(r_fd).expect("Failed to close original read file descriptor");
    let shard_pipes = vec![unsafe { fs::File::from_raw_fd(dup_fd) }];

    let stats = serve(shard_pipes, ced_listener, criu_listener);
    let _stats_ref = stats.as_ref().unwrap();

    Ok(())
}

fn do_main() -> Result<()> {
    let opts: Opts = Opts::from_args();

    let dir_path_string = &opts.dir;
    let dir_path = Path::new(&dir_path_string);

    match opts.operation {
        Operation::Capture => do_capture(dir_path),
        Operation::Serve => do_serve(dir_path),
    }?;
    Ok(())
}

fn main() {
    if let Err(e) = do_main() {
        eprintln!("criu-image-streamer Error: {:#}", e);
    }
}
