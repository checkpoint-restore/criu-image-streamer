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

#[derive(Default)]
pub struct Store {
    files: HashMap<Box<str>, File>,
}

impl Store {
    pub fn remove(&mut self, filename: &str) -> Option<File> {
        self.files.remove(filename)
    }

    pub fn remove_by_prefix(&mut self, filename_prefix: &str) -> Option<(Box<str>, File)> {
        // find key with prefix
        let key = self.files.keys()
            .find(|key| key.starts_with(filename_prefix))
            .cloned();
        // if key found, remove entry and return key + file
        key.and_then(|ref k| self.files.remove_entry(k))
    }
}

impl ImageStore for Store {
    type File = File;

    fn create(&mut self, _filename: &str) -> Result<Self::File> {
        Ok(File::new_small())
    }

    fn insert(&mut self, filename: impl Into<Box<str>>, file: File) {
        let filename = filename.into();
        assert!(!self.files.contains_key(&filename), "Image file {} is being overwritten", filename);

        // We don't need to shrink our file. If it's a small one, then it's already small
        // enough (we used reserve_exact()). For a large file, we could mremap() the last
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
        // This function is always used to convert a small file into a large
        // file. There's no panic as `init_data` is a most PAGE_SIZE=4KB, which
        // fits into the mmap buffer (size 10MB).
        assert!(init_data.len() <= MAX_LARGE_CHUNK_SIZE);

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

    pub fn copy_from_reader(&mut self, reader: &mut impl Read, size: usize) -> Result<usize> {
        // reserve_chunk() upgrades a small file to a large file if needed.
        self.reserve_chunk(size);

        Ok(match self {
            Small(chunk) => {
                reader.take(size as u64).read_to_end(chunk)
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
                reader.read_exact(&mut chunk[current_offset..])
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

    pub fn reader(&self) -> FileReader {
        let chunks = match self {
            Small(chunk) => vec![&chunk[..]].into_iter().collect(),
            Large(chunks) => chunks.iter().map(|chunk| &chunk[..]).collect(),
        };
        FileReader { chunks }
    }
}

impl ImageFile for File {
    fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, mut size: usize) -> Result<()> {
        while size > 0 {
            let written = self.copy_from_reader(shard_pipe, size)?;
            size -= written;
        }

        Ok(())
    }
}

pub struct FileReader<'a> {
    chunks: VecDeque<&'a [u8]>,
}

impl<'a> Read for FileReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if let Some(chunk) = self.chunks.front_mut() {
            let result = chunk.read(buf);
            if chunk.is_empty() {
                self.chunks.pop_front();
            }
            result
        } else {
            Ok(0)
        }
    }
}

impl Write for File {
    fn write(&mut self, mut buf: &[u8]) -> IoResult<usize> {
        let len = buf.len();
        // unwrap() is safe: the provided reader is a slice, so we are
        // guaranteed no read errors. We are writing to an in-memory buffer, so
        // we don't have write errors.
        Ok(self.copy_from_reader(&mut buf, len).unwrap())
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}
