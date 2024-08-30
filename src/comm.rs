use anyhow::Result;
use image_streamer::{
    capture::capture,
    criu_connection::CriuListener,
    extract::serve,
    unix_pipe::UnixPipe,
    util::create_dir_all,
};
use libc::dup;
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use nix::unistd::{close, pipe};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::io::{FromRawFd, RawFd},
    path::{Path, PathBuf},
    thread,
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

fn do_capture(dir_path: &Path) -> Result<()> {
    let (r_fd, w_fd): (RawFd, RawFd) = pipe()?;

    let dup_fd = unsafe { dup(w_fd) };
    close(w_fd).expect("Failed to close original write file descriptor");
    let shard_pipes: Vec<UnixPipe> = {
        vec![unsafe { fs::File::from_raw_fd(dup_fd) }]
    };

    let _ret = create_dir_all(dir_path);

    let gpu_listener = CriuListener::bind_for_capture(dir_path, "gpu-capture.sock")?;
    let criu_listener = CriuListener::bind_for_capture(dir_path, "streamer-capture.sock")?;
    let ced_listener = CriuListener::bind_for_capture(dir_path, "ced-capture.sock")?;

    let output_file_path = dir_path.join("img.lz4");
    let output_file = File::create(output_file_path)?;
    eprintln!("r");

    thread::spawn(move || {
        let stats = capture(shard_pipes, gpu_listener, criu_listener, ced_listener);
        let _stats_ref = stats.as_ref().unwrap();
    });
    let mut input_file = unsafe { File::from_raw_fd(r_fd) };
    let mut encoder = FrameEncoder::new(output_file);
    let _copy_result = io::copy(&mut input_file, &mut encoder);
    let _lz4_result = encoder.finish();
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
