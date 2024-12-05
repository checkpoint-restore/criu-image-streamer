//  Copyright 2024 Cedana.
//
//  Modifications licensed under the Apache License, Version 2.0.

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

use chrono::{DateTime, Local};
use prost::Message;
use std::{
    mem::size_of,
    os::unix::net::UnixStream,
    os::unix::io::{RawFd, AsRawFd},
    io::{Read, Write},
    path::Path,
    time::SystemTime,
    fs,
};
use nix::{
    poll::{poll, PollFd, PollFlags},
    sys::socket::{ControlMessageOwned, MsgFlags, recvmsg},
    sys::uio::IoVec,
    unistd::{sysconf, SysconfVar},
};
use bytes::{BytesMut, Buf, BufMut};
use serde::Serialize;
use anyhow::{Result, Context};

pub const KB: usize = 1024;
pub const MB: usize = 1024*1024;
pub const EOF_ERR_MSG: &str = "EOF unexpectedly reached";

lazy_static::lazy_static! {
    pub static ref PAGE_SIZE: usize = sysconf(SysconfVar::PAGE_SIZE)
        .expect("Failed to determine PAGE_SIZE")
        .expect("Failed to determine PAGE_SIZE") as usize;
}

pub fn timestamp() -> String {
    let system_time = SystemTime::now();
    let datetime: DateTime<Local> = system_time.into();
    datetime.format("%H:%M:%S%.6f").to_string()
}

#[macro_export]
macro_rules! prnt {
    ($msg:expr) => {{
        print!("\x1b[90m{}\x1b[0m [\x1b[1m{}:{}\x1b[0m] ", crate::util::timestamp(), file!(), line!());
        println!($msg);
    }};
    ($msg:expr, $($arg:expr),*) => {{
        print!("\x1b[90m{}\x1b[0m [\x1b[1m{}:{}\x1b[0m] ", crate::util::timestamp(), file!(), line!());
        print!($msg, $($arg),*);
        println!();
    }};
}

/// read_bytes_next() attempts to read exactly the number of bytes requested.
/// If we are at EOF, it returns Ok(None).
/// If it can read the number of bytes requested, it returns Ok(bytes_requested).
/// Otherwise, it returns Err("EOF error").
pub fn read_bytes_next<S: Read>(src: &mut S, len: usize) -> Result<Option<BytesMut>> {
    let mut buf = Vec::with_capacity(len);

    src.take(len as u64).read_to_end(&mut buf).context("Failed to read protobuf")?;
    Ok(match buf.len() {
        0 => None,
        l if l == len => Some(buf[..].into()),
        _ => bail!(EOF_ERR_MSG),
    })
}

pub fn read_bytes_next_breaking(src: &mut UnixStream, len: usize, file_path: &Path) -> Result<Option<BytesMut>> {
    let fd = src.as_raw_fd();
    let mut buf = vec![0u8; len];

    loop {
        let mut poll_fd = [PollFd::new(fd, PollFlags::POLLIN)];
        let poll_result = poll(&mut poll_fd, 10)?;
        if poll_result > 0 {
            if let Some(poll_fd) = poll_fd.get(0) {
                if poll_fd.revents().unwrap().contains(PollFlags::POLLIN) {
                    let read_bytes = src.read(&mut buf).context("Failed to read socket")?;
                    if read_bytes > 0 {
                        return Ok(Some(buf[..read_bytes].into()));
                    }
                }
            }
        }

        if file_path.exists() {
            prnt!("ready path exists, breaking from read_bytes_next_breaking");
            return Ok(None);
        }
    }
}

/// pb_read_next() is useful to iterate through a stream of protobuf objects.
/// It returns Ok(obj) for each object to be read, and Ok(None) when EOF is reached.
/// It returns an error if an object is only partially read, or any deserialization error.
pub fn pb_read_next<S: Read, T: Message + Default>(src: &mut S) -> Result<Option<(T, usize)>> {
    Ok(match read_bytes_next(src, size_of::<u32>())? {

        None => None,
        Some(mut size_buf) => {
            let size = size_buf.get_u32_le() as usize;
            assert!(size < 10*KB, "Would read a protobuf of size >10KB. Something is wrong");
            let buf = read_bytes_next(src, size)?.ok_or_else(|| anyhow!(EOF_ERR_MSG))?;
            let bytes_read = size_of::<u32>() + size_buf.len() + buf.len();
            Some((T::decode(buf)?, bytes_read))
        }
    })
}

pub fn pb_read_next_breaking<T: Message + Default>(src: &mut UnixStream, ready_path: &Path) -> Result<Option<(T, usize)>> {
    Ok(match read_bytes_next_breaking(src, size_of::<u32>(), ready_path)? {

        None => None,
        Some(mut size_buf) => {
            let size = size_buf.get_u32_le() as usize;
            assert!(size < 10*KB, "Would read a protobuf of size >10KB. Something is wrong");
            let buf = read_bytes_next(src, size)?.ok_or_else(|| anyhow!(EOF_ERR_MSG))?;
            let bytes_read = size_of::<u32>() + size_buf.len() + buf.len();
            Some((T::decode(buf)?, bytes_read))
        }
    })
}

pub fn pb_read<S: Read, T: Message + Default>(src: &mut S) -> Result<T> {
    Ok(match pb_read_next(src)? {
        None => bail!(EOF_ERR_MSG),
        Some((obj, _size)) => obj,
    })
}

pub fn pb_write<S: Write, T: Message>(dst: &mut S, msg: &T) -> Result<usize> {
    let msg_size = msg.encoded_len();
    let mut buf = BytesMut::with_capacity(size_of::<u32>() + msg_size);
    assert!(msg_size < 10*KB, "Would serialize a protobuf of size >10KB. Something is wrong");
    buf.put_u32_le(msg_size as u32);

    msg.encode(&mut buf).context("Failed to encode protobuf")?;
    dst.write_all(&buf).context("Failed to write protobuf")?;

    Ok(buf.len())
}

pub fn recv_fd(socket: &mut UnixStream) -> Result<RawFd> {
    let mut cmsgspace = nix::cmsg_space!([RawFd; 1]);

    let msg = recvmsg(socket.as_raw_fd(),
                      &[IoVec::from_mut_slice(&mut [0])],
                      Some(&mut cmsgspace),
                      MsgFlags::empty())
        .context("Failed to read fd from socket")?;

    Ok(match msg.cmsgs().next() {
        Some(ControlMessageOwned::ScmRights(fds)) if fds.len() == 1 => fds[0],
        _ => bail!("No fd received"),
    })
}

pub fn create_dir_all(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory {}", dir.display()))
}

#[derive(Serialize)]
pub struct ShardStat {
    pub size: u64,
    pub transfer_duration_millis: u64,
}
