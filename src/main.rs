//  Copyright 2020 Two Sigma Investments, LP.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.

//! Executable entry point. Imports lib.rs via the criu_image_streamer crate.

// Unless we are in release mode, allow dead code, unused imports and variables,
// it makes development more enjoyable.
#![cfg_attr(debug_assertions, allow(dead_code, unused_imports, unused_variables))]

#[macro_use]
extern crate anyhow;

use std::{
    os::unix::io::FromRawFd,
    path::PathBuf,
    fs,
};
use structopt::{StructOpt, clap::AppSettings};
use criu_image_streamer::{
    unix_pipe::{UnixPipe, UnixPipeImpl},
    capture::capture,
    extract::extract,
};
use nix::unistd::dup;
use anyhow::{Result, Context};

fn parse_ext_fd(s: &str) -> Result<(String, i32)> {
    let mut parts = s.split(':');
    Ok(match (parts.next(), parts.next()) {
        (Some(filename), Some(fd)) => {
            let filename = filename.to_string();
            let fd = fd.parse().context("Provided ext fd is not an integer")?;
            (filename, fd)
        },
        _ => bail!("Format is filename:fd")
    })
}

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
    // The short option -D mimics CRIU's short option for its --images-dir argument.
    #[structopt(short = "D", long)]
    images_dir: PathBuf,

    /// File descriptors of shards. Multiple fds may be passed as a comma separated list.
    /// Defaults to 0 or 1 depending on the operation.
    // require_delimiter is set to avoid clap's non-standard way of accepting lists.
    #[structopt(short, long, require_delimiter = true)]
    shard_fds: Vec<i32>,

    /// External files to incorporate/extract in/from the image. Format is filename:fd
    /// where filename corresponds to the name of the file, fd corresponds to the pipe
    /// sending or receiving the file content. Multiple external files may be passed as
    /// a comma separated list.
    #[structopt(short, long, parse(try_from_str=parse_ext_fd), require_delimiter = true)]
    ext_file_fds: Vec<(String, i32)>,

    /// File descriptor where to report progress. Defaults to 2.
    // The default being 2 is a bit of a lie. We dup(STDOUT_FILENO) due to ownership issues.
    #[structopt(short, long)]
    progress_fd: Option<i32>,

    #[structopt(subcommand)]
    operation: Operation,
}

#[derive(StructOpt, PartialEq, Debug)]
enum Operation {
    /// Capture a CRIU image
    Capture,

    /// Extract a captured CRIU image
    Extract {
        /// Buffer the image in memory and serve to CRIU
        #[structopt(long)]
        serve: bool,
    }
}

fn main() -> Result<()> {
    let opts: Opts = Opts::from_args();

    let progress_pipe = {
        let progress_fd = match opts.progress_fd {
            Some(fd) => fd,
            None => dup(libc::STDERR_FILENO)?
        };
        unsafe { fs::File::from_raw_fd(progress_fd) }
    };

    let shard_pipes =
        if !opts.shard_fds.is_empty() {
            opts.shard_fds
        } else {
            match opts.operation {
                Operation::Capture => vec![dup(libc::STDOUT_FILENO)?],
                Operation::Extract{..} => vec![dup(libc::STDIN_FILENO)?],
            }
        }.into_iter()
            .map(UnixPipe::new)
            .collect::<Result<_>>()
            .context("Image shards (input/output) must be pipes. \
                      You may use `cat` or `pv` (faster) to create one.")?;

    let ext_file_pipes = opts.ext_file_fds.into_iter()
            .map(|(filename, fd)| Ok((filename, UnixPipe::new(fd)?)))
            .collect::<Result<_>>()?;

    match opts.operation {
        Operation::Capture =>
            capture(&opts.images_dir, progress_pipe, shard_pipes, ext_file_pipes),
        Operation::Extract { serve } =>
            extract(&opts.images_dir, progress_pipe, shard_pipes, ext_file_pipes, serve),
    }
}


#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn test_capture_basic() {
        assert_eq!(Opts::from_iter(&vec!["prog", "--images-dir", "imgdir", "capture"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![],
                ext_file_fds: vec![],
                progress_fd: None,
                operation: Operation::Capture,
            })
    }

    #[test]
    fn test_extract_basic() {
        assert_eq!(Opts::from_iter(&vec!["prog", "-D", "imgdir", "extract"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![],
                ext_file_fds: vec![],
                progress_fd: None,
                operation: Operation::Extract { serve: false },
            })
    }

    #[test]
    fn test_extract_serve() {
        assert_eq!(Opts::from_iter(&vec!["prog", "-D", "imgdir", "extract", "--serve"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![],
                ext_file_fds: vec![],
                progress_fd: None,
                operation: Operation::Extract { serve: true },
            })
    }


    #[test]
    fn test_shards_fds() {
        assert_eq!(Opts::from_iter(&vec!["prog", "--images-dir", "imgdir", "--shard-fds", "1,2,3", "capture"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![1,2,3],
                ext_file_fds: vec![],
                progress_fd: None,
                operation: Operation::Capture,
            })
    }

    #[test]
    fn test_ext_files() {
        assert_eq!(Opts::from_iter(&vec!["prog", "--images-dir", "imgdir", "--ext-file-fds", "file1:1,file2:2", "capture"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![],
                ext_file_fds: vec![(String::from("file1"), 1), (String::from("file2"), 2)],
                progress_fd: None,
                operation: Operation::Capture,
            })
    }

    #[test]
    fn test_progess_fd() {
        assert_eq!(Opts::from_iter(&vec!["prog", "--images-dir", "imgdir", "--progress-fd", "3", "capture"]),
            Opts {
                images_dir: PathBuf::from("imgdir"),
                shard_fds: vec![],
                ext_file_fds: vec![],
                progress_fd: Some(3),
                operation: Operation::Capture,
            })
    }
}

