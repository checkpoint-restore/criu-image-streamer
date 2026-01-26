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
    ptr,
    slice,
    ops::{Drop, Deref, DerefMut},
};
use nix::sys::mman::{mmap, munmap, ProtFlags, MapFlags};
use core::ffi::c_void;

/// `MmapBuf` is semantically a `Vec<u8>` backed by an mmap region.
/// See the discussion in `image_store::mem` for why it is useful.
///
/// We don't use the memmap create because it doesn't offer a len+capacity abstraction. We'd have
/// to do a wrapper on their `MmapMap` type. That doesn't buy us much code reuse.
pub struct MmapBuf {
    addr: ptr::NonNull<u8>,
    len: usize,
    capacity: usize,
}

#[allow(clippy::len_without_is_empty)]
impl MmapBuf {
    pub fn with_capacity(capacity: usize) -> Self {
        unsafe {
            let addr = mmap(ptr::null_mut(), capacity,
                            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                            MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS,
                            -1, 0,
                            ).expect("mmap() failed") as *mut u8;
            let addr = ptr::NonNull::new_unchecked(addr);
            Self { addr, len: 0, capacity }
        }
    }

    pub fn resize(&mut self, len: usize) {
        assert!(len <= self.capacity);
        self.len = len;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Deref for MmapBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr.as_ptr(), self.len) }
    }
}

impl DerefMut for MmapBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr.as_ptr(), self.len) }
    }
}

impl Drop for MmapBuf {
    fn drop(&mut self) {
        unsafe {
            munmap(self.addr.as_ptr() as *mut c_void, self.capacity).expect("munmap() failed");
        }
    }
}
