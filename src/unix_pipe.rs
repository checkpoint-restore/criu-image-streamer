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
    os::unix::io::{RawFd, FromRawFd, AsRawFd},
    sync::Once,
    fs,
};
use nix::{
    sys::stat::{fstat, SFlag},
    fcntl::{fcntl, FcntlArg},
    fcntl::{vmsplice, splice, SpliceFFlags},
    sys::uio::IoVec,
    errno::Errno,
    Error,
};
use crate::util::PAGE_SIZE;
use anyhow::{Context, Result};

/// Unix pipes are regular `fs::File`. To add pipe specific functionalities, we have three options:
/// 1) Use unscoped functions. Less pleasant to use.
/// 2) Define a new struct like `struct UnixPipe(fs::File)`. But this has the disadvantage that we
///    can no longer manipulate `UnixPipe` as a file when we wish to do so. For example, we'd have
///    to reimplement the `Reader` and `Writer` trait.
/// 3) Define a new trait `UnixPipeImpl`. This has the downside that we need to import the
///    `UnixPipeImpl` everywhere we want to use the `UnixPipe` features. Not a terrible downside,
///    so we go with this.

pub type UnixPipe = fs::File;

pub trait UnixPipeImpl: Sized {
    fn new(fd: RawFd) -> Result<Self>;
    fn fionread(&self) -> Result<i32>;
    fn set_capacity(&mut self, capacity: i32) -> nix::Result<()>;
    fn set_capacity_no_eperm(&mut self, capacity: i32) -> Result<()>;
    fn set_best_capacity(pipes: &mut [Self], max_capacity: i32) -> Result<i32>;
    fn splice_all(&mut self, dst: &mut fs::File, len: usize) -> Result<()>;
    fn vmsplice_all(&mut self, data: &[u8]) -> Result<()>;
}

impl UnixPipeImpl for UnixPipe {
    fn new(fd: RawFd) -> Result<Self> {
        fn ensure_pipe_type(fd: RawFd) -> Result<()> {
            let stat = fstat(fd).with_context(|| format!("fstat() failed on fd {}", fd))?;
            let is_pipe = (SFlag::S_IFMT.bits() & stat.st_mode) == SFlag::S_IFIFO.bits();
            ensure!(is_pipe, "fd {} is not a pipe", fd);
            Ok(())
        }

        ensure_pipe_type(fd)?;
        unsafe { Ok(fs::File::from_raw_fd(fd)) }
    }

    fn fionread(&self) -> Result<i32> {
        // fionread() is defined as an int in the kernel, hence the signed i32
        nix::ioctl_read_bad!(_fionread, libc::FIONREAD, i32);

        let mut result = 0;
        unsafe { _fionread(self.as_raw_fd(), &mut result) }
            .with_context(|| format!("Failed to get pipe content size via fionread() on fd {}",
                                     self.as_raw_fd()))?;
        Ok(result)
    }

    fn set_capacity(&mut self, capacity: i32) -> nix::Result<()> {
        fcntl(self.as_raw_fd(), FcntlArg::F_SETPIPE_SZ(capacity)).map(|_| ())
    }

    // Same as set_capacity(), except EPERM errors are ignored.
    fn set_capacity_no_eperm(&mut self, capacity: i32) -> Result<()> {
        match self.set_capacity(capacity) {
            Err(Error::Sys(Errno::EPERM)) => {
                warn_once_capacity_eperm();
                Ok(())
            }
            other => other,
        }?;
        Ok(())
    }

    /// Sets the capacity of many pipes. /proc/sys/fs/pipe-user-pages-{hard,soft} may be non-zero,
    /// preventing setting the desired capacity. If we can't set the provided `max_capacity`, then
    /// we try with a lower capacity. Eventually we will succeed.
    /// Returns the actual capacity of the pipes.
    fn set_best_capacity(pipes: &mut [Self], max_capacity: i32) -> Result<i32> {
        let mut capacity = max_capacity;
        loop {
            match pipes.iter_mut().try_for_each(|pipe| pipe.set_capacity(capacity)) {
                Err(Error::Sys(Errno::EPERM)) => {
                    warn_once_capacity_eperm();
                    assert!(capacity > *PAGE_SIZE as i32);
                    capacity /= 2;
                    continue;
                }
                Err(e) => return Err(anyhow!(e)),
                Ok(()) => return Ok(capacity),
            };
        }
    }

    fn splice_all(&mut self, dst: &mut fs::File, len: usize) -> Result<()> {
        let mut to_write = len;

        while to_write > 0 {
            let written = splice(self.as_raw_fd(), None, dst.as_raw_fd(), None,
                                 to_write, SpliceFFlags::SPLICE_F_MORE)
                .with_context(|| format!("splice() failed fd {} -> fd {}",
                                         self.as_raw_fd(), dst.as_raw_fd()))?;
            ensure!(written > 0, "Reached EOF during splice() on fd {}", self.as_raw_fd());
            to_write -= written;
        }

        Ok(())
    }

    fn vmsplice_all(&mut self, data: &[u8]) -> Result<()> {
        let mut to_write = data.len();
        let mut offset = 0;

        while to_write > 0 {
            let in_iov = IoVec::from_slice(&data[offset..]);
            let written = vmsplice(self.as_raw_fd(), &[in_iov], SpliceFFlags::SPLICE_F_GIFT)
                .with_context(|| format!("vmsplice() failed on fd {}", self.as_raw_fd()))?;
            assert!(written > 0, "vmsplice() returned 0");

            to_write -= written;
            offset += written;
        }

        Ok(())
    }
}

fn warn_once_capacity_eperm() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!("Cannot set pipe size as desired (EPERM). \
                   Continuing with smaller pipe sizes but performance may be reduced. \
                   See the Deploy section in the criu-image-streamer README for a remedy.");
    });
}
