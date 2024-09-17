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
    os::unix::net::{UnixStream, UnixListener},
    os::unix::fs::PermissionsExt,
    os::unix::io::{RawFd, AsRawFd},
    path::Path,
    fs,
};
use crate::{
    criu,
    util::{pb_write, recv_fd, pb_read_next},
    unix_pipe::{UnixPipe, UnixPipeImpl},
};
use anyhow::{Result, Context};

/// The role of the `CriuListener` and `CriuConnection` is to handle communication with CRIU and
/// Cedana over the image sockets.

pub struct CriuListener {
    listener: UnixListener,
}

impl CriuListener {
    pub fn bind(images_dir: &Path, socket_name: &str) -> Result<Self> {
        // 1) We unlink the socket path to avoid EADDRINUSE on bind() if it already exists.
        // 2) We ignore the unlink error because we are most likely getting a -ENOENT.
        //    It is safe to do so as correctness is not impacted by unlink() failing.
        let socket_path = images_dir.join(socket_name);
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("Failed to bind socket to {}", socket_path.display()))?;

        let mut permissions = fs::metadata(&socket_path)?.permissions();
        permissions.set_mode(0o777);
        fs::set_permissions(&socket_path, permissions)?;

        Ok(Self { listener })
    }

    // into_accept() drops the listener. There is no need for having multiple CRIU connections,
    // so we close the listener here.
    pub fn into_accept(self) -> Result<CriuConnection> {
        let (socket, _) = self.listener.accept()?;
        Ok(CriuConnection { socket })
    }
}

pub struct CriuConnection {
    socket: UnixStream,
}

impl CriuConnection {
    /// Read and return the next file request. If reached EOF, returns Ok(None).
    pub fn read_next_file_request(&mut self) -> Result<Option<String>> {
        Ok(pb_read_next(&mut self.socket)?
            .map(|(req, _): (criu::ImgStreamerRequestEntry, _)| req.filename))
    }

    /// Returns the data pipe that is used to transfer the file.
    pub fn recv_pipe(&mut self) -> Result<UnixPipe> {
        UnixPipe::new(recv_fd(&mut self.socket)?)
    }

    /// During restore, CRIU requests image files that may or may not exist.
    /// We must let CRIU know if we hold has the requested file in question.
    /// It is done via `send_file_reply()`. Not used during checkpointing.
    pub fn send_file_reply(&mut self, exists: bool) -> Result<()> {
        pb_write(&mut self.socket, &criu::ImgStreamerReplyEntry { exists })?;
        Ok(())
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}
