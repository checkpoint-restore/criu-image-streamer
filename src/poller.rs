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
    os::unix::io::RawFd,
    convert::TryFrom,
    ops::Drop,
};
use slab::Slab;
use nix::{
    sys::epoll::{epoll_create, epoll_ctl, epoll_wait, EpollOp, EpollEvent},
    unistd::close,
    errno::Errno,
    Error,
};
pub use nix::sys::epoll::EpollFlags;
use anyhow::{Context, Result};

/// `Poller` provides an easy-to-use interface to epoll(). It associates file descriptor with
/// objects. When a file descriptor is ready, the poller returns a reference to the corresponding
/// object via poll().
/// There should be a crate with this functionality. Either we didn't look well enough, or we
/// should publish a crate, because it seems useful beyond this project,
pub struct Poller<T> {
    epoll_fd: RawFd,
    slab: Slab<(RawFd, T)>,
    pending_events: Vec<EpollEvent>,
}

pub type Key = usize;

impl<T> Poller<T> {
    pub fn new() -> Result<Self> {
        let epoll_fd = epoll_create().context("Failed to create epoll")?;
        let slab = Slab::new();
        let pending_events = Vec::new();

        Ok(Self { epoll_fd, slab, pending_events })
    }

    pub fn add(&mut self, fd: RawFd, obj: T, flags: EpollFlags) -> Result<Key> {
        let entry = self.slab.vacant_entry();
        let key = entry.key();
        let mut event = EpollEvent::new(flags, u64::try_from(key).unwrap());
        epoll_ctl(self.epoll_fd, EpollOp::EpollCtlAdd, fd, &mut event)
            .context("Failed to add fd to epoll")?;
        entry.insert((fd, obj));
        Ok(key)
    }

    pub fn remove(&mut self, key: Key) -> Result<T> {
        let (fd, obj) = self.slab.remove(key);
        epoll_ctl(self.epoll_fd, EpollOp::EpollCtlDel, fd, None)
            .context("Failed to remove fd from epoll")?;
        Ok(obj)
    }

    /// Returns None when the poller has no file descriptors to track.
    /// Otherwise, blocks and returns a reference to the next ready object.
    ///
    /// `capacity` corresponds to the number of file descriptors that can be returned by a single
    /// system call.
    pub fn poll(&mut self, capacity: usize) -> Result<Option<(Key, &mut T)>> {
        if self.slab.is_empty() {
            return Ok(None);
        }

        if self.pending_events.is_empty() {
            self.pending_events.resize(capacity, EpollEvent::empty());

            let timeout = -1;
            let num_ready_fds = epoll_wait_no_intr(self.epoll_fd, &mut self.pending_events, timeout)
                .context("Failed to wait on epoll")?;

            // We don't use a timeout (-1), and we have events registered (slab is not empty)
            // so we should have a least one fd ready.
            assert!(num_ready_fds > 0);

            self.pending_events.truncate(num_ready_fds);
        }

        let event = self.pending_events.pop().unwrap();
        let key = event.data() as usize;
        let (_fd, obj) = &mut self.slab[key];
        Ok(Some((key, obj)))
    }
}

impl<T> Drop for Poller<T> {
    fn drop(&mut self) {
        close(self.epoll_fd).expect("Failed to close epoll");
    }
}

pub fn epoll_wait_no_intr(epoll_fd: RawFd, events: &mut [EpollEvent], timeout_ms: isize)
    -> nix::Result<usize>
{
    loop {
        match epoll_wait(epoll_fd, events, timeout_ms) {
            Err(Error::Sys(Errno::EINTR)) => continue,
            other => return other,
        }
    }
}
