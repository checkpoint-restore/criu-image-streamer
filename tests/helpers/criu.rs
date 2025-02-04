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

use std::{
    os::unix::net::UnixStream,
    os::unix::io::AsRawFd,
    io::Read,
    path::PathBuf,
};
use anyhow::{Result, Context};
use cedana_image_streamer::{
    criu,
    util::{pb_read, pb_write},
    unix_pipe::UnixPipe,
};
use crate::helpers::util::*;

/// For test purposes, we implement a CRIU simulator
pub struct Criu {
    socket: UnixStream,
}

impl Criu {
    pub fn connect(socket_path: PathBuf) -> Result<Self> {
        let socket = UnixStream::connect(&socket_path)
            .with_context(|| format!("Failed to connect to {}", socket_path.display()))?;
        Ok(Self { socket })
    }

    fn read_file_reply(&mut self) -> Result<bool> {
        let reply: criu::ImgStreamerReplyEntry = pb_read(&mut self.socket)?;
        Ok(reply.exists)
    }

    pub fn finish(&mut self) -> Result<()> {
        pb_write(&mut self.socket, &criu::ImgStreamerRequestEntry { filename: "stop-listener".to_string() })?;
        // drops the connection
        Ok(())
    }

    pub fn write_img_file(&mut self, filename: &str) -> Result<UnixPipe> {
        let filename = filename.to_string();
        pb_write(&mut self.socket, &criu::ImgStreamerRequestEntry { filename })?;
        let (pipe_r, pipe_w) = new_pipe();
        send_fd(&mut self.socket, pipe_r.as_raw_fd())?;
        Ok(pipe_w)
    }

    pub fn maybe_read_img_file(&mut self, filename: &str) -> Result<Option<UnixPipe>> {
        let filename = filename.to_string();
        pb_write(&mut self.socket, &criu::ImgStreamerRequestEntry { filename })?;

        if self.read_file_reply()? {
            let (pipe_r, pipe_w) = new_pipe();
            send_fd(&mut self.socket, pipe_w.as_raw_fd())?;
            Ok(Some(pipe_r))
        } else {
            Ok(None)
        }
    }

    pub fn read_img_file(&mut self, filename: &str) -> Result<UnixPipe> {
        self.maybe_read_img_file(filename)?
            .ok_or_else(|| anyhow!("Requested file does not exists"))
    }

    pub fn read_img_file_into_vec(&mut self, filename: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.read_img_file(filename)?.read_to_end(&mut buf)?;
        Ok(buf)
    }
}
