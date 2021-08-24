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

use super::{ImageStore, ImageFile};
use anyhow::{Context, Result};
use std::{
    fs,
    collections::{VecDeque, HashMap},
    io::{Read, Write, Result as IoResult},
    cmp::min,
};
use crate::{
    util::{MB, PAGE_SIZE},
    unix_pipe::{UnixPipe, UnixPipeImpl},
    mmap_buf::MmapBuf,
};

/// CRIU can generate many small files (e.g., core-*.img files), and a few large ones (e.g.,
/// pages-*.img files). For efficiency, we store small files in a regular `Vec<u8>`. When it
/// grows bigger than a page size, we switch to a large file implementation that uses mmap
/// chunks to store its content.
///
/// This is beneficial for avoiding blowing up our memory budget: CRIU copies data from the
/// image pipe. As CRIU copies data from our memory space, we need to release the memory that
/// we are holding. The default memory allocator is unpredictable when it comes to using brk(),
/// or mmap(), risking an Out-Of-Memory situation. Which is why we use our `MmapBuf`
/// implementation.
///
/// The large chunk size should not be too large (e.g., 100MB) as the chunk size directly
/// increases our memory overhead while transferring data to CRIU: while CRIU is duplicates the
/// chunk data into its memory space, the chunk remains allocated until we get to the next chunk.

const MAX_LARGE_CHUNK_SIZE: usize = 10*MB;
static MAX_SMALL_CHUNK_SIZE: &PAGE_SIZE = &PAGE_SIZE;

pub struct Store {
    files: HashMap<Box<str>, File>,
}

impl Store {
    pub fn new() -> Self {
        Self { files: HashMap::new() }
    }

    pub fn remove(&mut self, filename: &str) -> Option<File> {
        self.files.remove(filename)
    }
}

impl ImageStore for Store {
    type File = File;

    fn create(&mut self, _filename: &str) -> Result<Self::File> {
        Ok(File::new_small())
    }

    fn close(&mut self, filename: Box<str>, file: File) {
        // We don't need to shrink our file. If it's a small one, then it's already small
        // enough (we used reserve_exact()). For a large file, We could mremap() the last
        // chunk, but that doesn't really help as we don't touch pages from the unused
        // capacity, which thus remains unallocated.
        self.files.insert(filename, file);
    }
}

pub enum File {
    Small(Vec<u8>),
    Large(VecDeque<MmapBuf>),
}

use File::*;

impl File {
    pub fn new_small() -> Self {
        Small(Vec::new())
    }

    fn large_from_slice(init_data: &[u8]) -> Self {
        let mut chunk = MmapBuf::with_capacity(MAX_LARGE_CHUNK_SIZE);
        chunk.resize(init_data.len());
        chunk.copy_from_slice(init_data);

        let mut chunks = VecDeque::new();
        chunks.push_back(chunk);
        Large(chunks)
    }

    fn reserve_chunk(&mut self, size_hint: usize) {
        match self {
            Small(chunk) => {
                if chunk.len() + size_hint > **MAX_SMALL_CHUNK_SIZE {
                    *self = Self::large_from_slice(chunk);
                } else {
                    chunk.reserve_exact(size_hint);
                }
            }
            Large(chunks) => {
                match chunks.back_mut() {
                    // We don't use `size_hint` here. The caller will top-up the current chunk,
                    // and call `reserve_chunk()` again.
                    Some(chunk) if chunk.len() < chunk.capacity() => {}
                    _ => chunks.push_back(MmapBuf::with_capacity(MAX_LARGE_CHUNK_SIZE)),
                }
            }
        }
    }

    fn write_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<usize> {
        // reserve_chunk() upgrades a small file to a large file if needed.
        self.reserve_chunk(size);

        Ok(match self {
            Small(chunk) => {
                shard_pipe.take(size as u64).read_to_end(chunk)
                    .context("Failed to read from shard")?;
                size
            }
            Large(chunks) => {
                // Safe to unwrap() as we called reserve_chunk().
                let chunk = chunks.back_mut().unwrap();

                let current_offset = chunk.len();
                let remaining_chunk_len = chunk.capacity() - current_offset;
                let to_read = min(size, remaining_chunk_len);

                chunk.resize(current_offset + to_read);
                shard_pipe.read_exact(&mut chunk[current_offset..])
                    .context("Failed to read from shard")?;
                to_read
            }
        })
    }

    pub fn drain(self, dst: &mut fs::File) -> Result<()> {
        match self {
            Small(chunk) => dst.write_all(&chunk)?,
            Large(chunks) => {
                for chunk in chunks {
                    // We can vmsplice() because the chunk is backed by our mmap buffer.
                    // It will be unmapped after the vmsplice guaranteeing that memory pages
                    // are not going to be recycled and modified, which is a problem for
                    // vmsplice().
                    dst.vmsplice_all(&chunk)?;
                }
            }
        };

        Ok(())
    }
}

impl ImageFile for File {
    fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, mut size: usize) -> Result<()> {
        while size > 0 {
            let written = self.write_from_pipe(shard_pipe, size)?;
            size -= written;
        }

        Ok(())
    }
}
