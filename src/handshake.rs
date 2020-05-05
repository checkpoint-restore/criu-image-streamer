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

use std::os::unix::net::UnixStream;
use crate::criu;
use crate::util::{pb_write, pb_read};
use anyhow::Result;

// These constants can be seen in the CRIU source code at criu/include/img-remote.h
const NULL_SNAPSHOT_ID: &str = "";
const FINISH: &str = "";
const PARENT_IMG: &str = "parent";

pub struct HandshakeContext {
    snapshot_ids: Vec<String>,
}

pub enum HandshakeResult {
    ReadFile(String),
    WriteFile(String),
    ImageEOF,
    SnapshotIdExchanged,
}

/// The role of the `HandshakeContext` is to handle communication with CRIU over the image socket.
/// The handshake determines what CRIU wants, and provides a `HandshakeResult` to the caller.
/// Note that we are storing `snapshot_ids` in the context. It seems that these are related to
/// incremental checkpoints where we may have a collection of snapshots. Even though we don't
/// use incremental checkpoints, CRIU needs it.

impl HandshakeContext {
    pub fn new() -> Self {
        Self { snapshot_ids: Vec::new() }
    }

    pub fn add_snapshot_id(&mut self, snapshot_id: String) {
        self.snapshot_ids.push(snapshot_id);
    }

    fn handshake_snapshot_id(&mut self, socket: &mut UnixStream, header: criu::LocalImageEntry)
        -> Result<HandshakeResult>
    {
        Ok(match header.name.as_str() {
            PARENT_IMG => match header.open_mode as i32 {
                libc::O_APPEND => {
                    let sie: criu::SnapshotIdEntry = pb_read(socket)?;
                    self.add_snapshot_id(sie.snapshot_id);
                    HandshakeResult::SnapshotIdExchanged
                }
                libc::O_RDONLY => {
                    Self::send_reply(socket, 0)?;
                    for snapshot_id in &self.snapshot_ids {
                        let entry = &criu::SnapshotIdEntry { snapshot_id: snapshot_id.into() };
                        pb_write(socket, entry)?;
                    }
                    HandshakeResult::SnapshotIdExchanged
                }
                _ => bail!("Unrecognized CRIU snapshot_id open_mode: {}", header.open_mode),
            }
            FINISH => HandshakeResult::ImageEOF,
            _ => bail!("Unrecognized CRIU snapshot_id name: {}", header.name),
        })
    }

    fn handshake_file(&self, header: criu::LocalImageEntry) -> Result<HandshakeResult> {
        let filename = header.name;
        Ok(match header.open_mode as i32 {
            libc::O_WRONLY => HandshakeResult::WriteFile(filename),
            libc::O_RDONLY => HandshakeResult::ReadFile(filename),
            _ => bail!("CRIU file open mode not recognized: {}", header.open_mode),
        })
    }

    pub fn handshake(&mut self, socket: &mut UnixStream) -> Result<HandshakeResult> {
        let header: criu::LocalImageEntry = pb_read(socket)?;

        match header.snapshot_id.as_str() {
            NULL_SNAPSHOT_ID => self.handshake_snapshot_id(socket, header),
            _ => self.handshake_file(header)
        }
    }

    pub fn send_reply(socket: &mut UnixStream, error: u32) -> Result<()> {
        pb_write(socket, &criu::LocalImageReplyEntry { error })?;
        Ok(())
    }
}
