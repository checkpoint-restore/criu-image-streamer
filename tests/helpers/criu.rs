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
    os::unix::io::AsRawFd,
    io::Read,
    path::PathBuf,
};
use anyhow::{Result, Context};
use criu_image_streamer::{
    criu,
    util::{pb_read, pb_read_next, pb_write},
    unix_pipe::UnixPipe,
};
use crate::helpers::util::*;

// These constants can be seen in the CRIU source code at criu/include/img-remote.h
const NULL_SNAPSHOT_ID: &str = "";
const FINISH: &str = "";
const PARENT_IMG: &str = "parent";

/// For test purposes, we implement a CRIU simulator
pub struct CRIU {
    socket_path: PathBuf,
}

impl CRIU {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    fn connect(&self, snapshot_id: &str, name: &str, open_mode: u32) -> Result<UnixStream> {
        let mut socket = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("Can't connect to {}", self.socket_path.display()))?;
        let snapshot_id = snapshot_id.to_string();
        let name = name.to_string();
        pb_write(&mut socket, &criu::LocalImageEntry { snapshot_id, name, open_mode })
            .with_context(|| "Can't write to CRIU socket")?;
        Ok(socket)
    }

    fn read_reply(socket: &mut UnixStream) -> Result<u32> {
        let reply: criu::LocalImageReplyEntry = pb_read(socket)?;
        return Ok(reply.error)
    }

    pub fn finish(&self) -> Result<()> {
        self.connect(NULL_SNAPSHOT_ID, FINISH, 0)?;
        Ok(())
    }

    pub fn append_snapshot_id(&self, snapshot_id: &str) -> Result<()> {
        let mut socket = self.connect(NULL_SNAPSHOT_ID, PARENT_IMG, libc::O_APPEND as u32)?;
        let snapshot_id = snapshot_id.to_string();
        pb_write(&mut socket, &criu::SnapshotIdEntry { snapshot_id })?;
        Ok(())
    }

    pub fn get_snapshot_ids(&self) -> Result<Vec<String>> {
        let mut socket = self.connect(NULL_SNAPSHOT_ID, PARENT_IMG, libc::O_RDONLY as u32)?;
        ensure!(Self::read_reply(&mut socket)? == 0, "Bad reply for get_snapshot_ids()");

        let mut snapshot_ids = Vec::new();
        while let Some((entry, _)) = pb_read_next::<_, criu::SnapshotIdEntry>(&mut socket)? {
            snapshot_ids.push(entry.snapshot_id)
        }

        Ok(snapshot_ids)
    }

    pub fn write_img_file(&self, snapshot_id: &str, filename: &str) -> Result<UnixPipe> {
        let mut socket = self.connect(snapshot_id, filename, libc::O_WRONLY as u32)?;
        let (pipe_r, pipe_w) = new_pipe();
        send_fd(&mut socket, pipe_r.as_raw_fd())?;
        Ok(pipe_w)
    }

    pub fn maybe_read_img_file(&self, snapshot_id: &str, filename: &str) -> Result<Option<UnixPipe>> {
        let mut socket = self.connect(snapshot_id, filename, libc::O_RDONLY as u32)?;

        match Self::read_reply(&mut socket)? as i32 {
            0 => {},
            libc::ENOENT => return Ok(None),
            _ => return Err(anyhow!("Bad response during read_img_file()")),
        }

        let (pipe_r, pipe_w) = new_pipe();
        send_fd(&mut socket, pipe_w.as_raw_fd())?;
        Ok(Some(pipe_r))
    }

    pub fn read_img_file(&self, snapshot_id: &str, filename: &str) -> Result<UnixPipe> {
        self.maybe_read_img_file(snapshot_id, filename)?
            .ok_or_else(|| anyhow!("Requested file does not exists"))
    }

    pub fn read_img_file_into_vec(&self, snapshot_id: &str, filename: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.read_img_file(snapshot_id, filename)?.read_to_end(&mut buf)?;
        Ok(buf)
    }
}
