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

use anyhow::{Result, Context};
use crate::{
    criu,
    unix_pipe::{UnixPipe, UnixPipeImpl},
    util::{pb_write, recv_fd, pb_read_next, pb_read_next_breaking},
};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    os::unix::io::{RawFd, AsRawFd},
    os::unix::net::{UnixStream, UnixListener},
    path::Path,
};

/// The role of the `EndpointListener` and `EndpointConnection` is to handle communication with 
/// endpoints cedana, cedana-gpu-controller, and criu over the image sockets.

pub struct EndpointListener {
    listener: UnixListener,
}

impl EndpointListener {
    pub fn bind(images_dir: &Path, socket_name: &str) -> Result<Self> {
        let socket_path = images_dir.join(socket_name);
        if let Err(e) = fs::remove_file(&socket_path) {
            if e.kind() != ErrorKind::NotFound { // Ignore DNE error
                eprintln!("Failed to remove file: {}", e); // Propagate other errors
            }
        }
        // 1) We unlink the socket path to avoid EADDRINUSE on bind() if it already exists.
        // 2) We ignore the unlink error because we are most likely getting a -ENOENT.
        //    It is safe to do so as correctness is not impacted by unlink() failing.
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("Failed to bind socket to {}", socket_path.display()))?;

        // Adjust permissions to allow cedana-gpu-controller to connect
        let mut permissions = fs::metadata(&socket_path)?.permissions();
        permissions.set_mode(0o666);
        fs::set_permissions(&socket_path, permissions)?;

        Ok(Self { listener })
    }

    // into_accept() drops the listener. There is no need for having multiple endpoint 
    // connections, so we close the listener here.
    pub fn into_accept(self) -> Result<EndpointConnection> {
        let (socket, _) = self.listener.accept()?;
        Ok(EndpointConnection { socket })
    }
}

pub struct EndpointConnection {
    socket: UnixStream,
}

impl EndpointConnection {
    /// Read and return the next file request. If reached EOF, returns Ok(None).
    pub fn read_next_file_request(&mut self) -> Result<Option<String>> {
        Ok(pb_read_next(&mut self.socket)?
            .map(|(req, _): (criu::ImgStreamerRequestEntry, _)| req.filename))
    }

    /// Same as read_next_file_request, but breaks when ready file appears.
    pub fn read_next_file_request_breaking(&mut self, ready_path: &Path) -> Result<Option<String>> {
        Ok(pb_read_next_breaking(&mut self.socket, ready_path)?
            .map(|(req, _): (criu::ImgStreamerRequestEntry, _)| req.filename))
    }

    /// Returns the data pipe that is used to transfer the file.
    pub fn recv_pipe(&mut self) -> Result<UnixPipe> {
        UnixPipe::new(recv_fd(&mut self.socket)?)
    }

    /// During restore, endpoints request image files that may or may not exist.
    /// We must let endpoints know if we hold has the requested file in question.
    /// It is done via `send_file_reply()`. Not used during checkpointing.
    pub fn send_file_reply(&mut self, exists: bool) -> Result<()> {
        pb_write(&mut self.socket, &criu::ImgStreamerReplyEntry { exists })?;
        Ok(())
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}
