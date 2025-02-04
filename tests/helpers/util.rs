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

use std::{
    os::unix::net::UnixStream,
    io::{Read, BufReader, BufRead},
    os::unix::io::{RawFd, AsRawFd},
};
use nix::{
    sys::socket::{ControlMessage, MsgFlags, sendmsg},
    sys::uio::IoVec,
    unistd,
};
use cedana_image_streamer::{
    unix_pipe::{UnixPipe, UnixPipeImpl},
    util::{KB, PAGE_SIZE},
};
use serde::Deserialize;
use anyhow::Result;

#[derive(Deserialize, Debug)]
pub struct Stats {
    pub shards: Vec<ShardStat>,
}
#[derive(Deserialize, Debug)]
pub struct ShardStat {
    pub size: u64,
    pub transfer_duration_millis: u128,
}

pub fn new_pipe() -> (UnixPipe, UnixPipe) {
    let (fd_r, fd_w) = unistd::pipe().expect("Failed to create UNIX pipe");
    let pipe_r = UnixPipe::new(fd_r).unwrap();
    let pipe_w = UnixPipe::new(fd_w).unwrap();
    (pipe_r, pipe_w)
}

pub fn read_line<R: Read>(progress: &mut BufReader<R>) -> Result<String> {
    let mut buf = String::new();
    progress.read_line(&mut buf)?;

    ensure!(buf.len() > 0, "EOF reached");
    ensure!(buf.chars().last() == Some('\n'), "no trailing \\n found");
    buf.pop(); // Removes the trailing '\n'

    Ok(buf)
}

pub fn read_stats<R: Read>(progress: &mut BufReader<R>) -> Result<Stats> {
    Ok(serde_json::from_str(&read_line(progress)?)?)
}

pub fn send_fd(socket: &mut UnixStream, fd: RawFd) -> Result<()> {
    sendmsg(socket.as_raw_fd(),
           &[IoVec::from_slice(&[0])],
           &[ControlMessage::ScmRights(&[fd])],
           MsgFlags::empty(),
           None)?;
    Ok(())
}

pub fn get_rand_vec(size: usize) -> Vec<u8> {
    let urandom = std::fs::File::open("/dev/urandom").expect("Failed to open /dev/urandom");
    let mut result = Vec::with_capacity(size);
    urandom.take(size as u64).read_to_end(&mut result).expect("Failed to read /dev/urandom");
    result
}

pub fn get_filled_vec(size: usize, value: u8) -> Vec<u8> {
    let mut vec = Vec::new();
    vec.resize(size, value);
    vec
}

pub fn get_resident_mem_size() -> usize {
    *PAGE_SIZE * procinfo::pid::statm_self().unwrap().resident
}

pub fn read_to_end_rate_limited(src: &mut UnixPipe, dst: &mut Vec<u8>,
                                max_rate_per_millis: usize) -> Result<usize> {
    use std::time::{Instant, Duration};

    let start_time = Instant::now();
    let mut total_size = 0;

    loop {
        let size = src.take(4*KB as u64).read_to_end(dst)?;
        if size == 0 {
            break; // EOF
        }
        total_size += size;

        let duration_goal = Duration::from_millis((total_size / max_rate_per_millis) as u64);
        if duration_goal > start_time.elapsed() {
            std::thread::sleep(duration_goal - start_time.elapsed())
        }
    }

    Ok(total_size)
}
