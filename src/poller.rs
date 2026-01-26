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
    os::unix::io::{RawFd, BorrowedFd},
    convert::TryFrom,
};
use slab::Slab;
use nix::{
    sys::epoll::{Epoll, EpollEvent, EpollCreateFlags, EpollTimeout},
    errno::Errno,
};
pub use nix::sys::epoll::EpollFlags;
use anyhow::{Context, Result};

/// `Poller` provides an easy-to-use interface to epoll(). It associates file descriptor with
/// objects. When a file descriptor is ready, the poller returns a reference to the corresponding
/// object via poll().
/// There should be a crate with this functionality. Either we didn't look well enough, or we
/// should publish a crate, because it seems useful beyond this project,
pub struct Poller<T> {
    epoll: Epoll,
    slab: Slab<(RawFd, T)>,
    pending_events: Vec<EpollEvent>,
}

pub type Key = usize;

impl<T> Poller<T> {
    pub fn new() -> Result<Self> {
        let epoll = Epoll::new(EpollCreateFlags::empty()).context("Failed to create epoll")?;
        let slab = Slab::new();
        let pending_events = Vec::new();

        Ok(Self { epoll, slab, pending_events })
    }

    pub fn add(&mut self, fd: RawFd, obj: T, flags: EpollFlags) -> Result<Key> {
        let entry = self.slab.vacant_entry();
        let key = entry.key();
        let event = EpollEvent::new(flags, u64::try_from(key).unwrap());
        // SAFETY: fd is valid for the duration of this call
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
        self.epoll.add(borrowed_fd, event)
            .context("Failed to add fd to epoll")?;
        entry.insert((fd, obj));
        Ok(key)
    }

    pub fn remove(&mut self, key: Key) -> Result<T> {
        let (fd, obj) = self.slab.remove(key);
        // SAFETY: fd is valid for the duration of this call
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
        self.epoll.delete(borrowed_fd)
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

            let num_ready_fds = epoll_wait_no_intr(&self.epoll, &mut self.pending_events)
                .context("Failed to wait on epoll")?;

            // We don't use a timeout (None = infinite), and we have events registered (slab is not empty)
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

pub fn epoll_wait_no_intr(epoll: &Epoll, events: &mut [EpollEvent]) -> nix::Result<usize> {
    loop {
        match epoll.wait(events, EpollTimeout::NONE) {
            Err(Errno::EINTR) => continue,
            other => return other,
        }
    }
}
