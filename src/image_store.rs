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

use anyhow::{Context, Result};
use crate::{
    unix_pipe::{UnixPipe, UnixPipeImpl},
    util::{MB, PAGE_SIZE},
};
use std::{
    collections::{VecDeque, HashMap},
    path::Path,
    cmp::min,
};

// This code is only used during image extraction.
//
// `ImageDeserializer` in extract.rs outputs the image into an image store, defined here.
// We have three image stores:
// * `fs::Store`, used to store an image on disk.
// * `mem::Store`, used to store an image in memory. This is useful to stream the image to
//   CRIU without touching disk.
// * `fs_overlay::Store`, used for bypassing certain files (like fs.tar) when extracting to memory.
//   These special files are passed via the "--ext-files-fds" option on the CLI.

// We use a `Box<str>` instead of `String` for filenames to reduce memory usage by 8 bytes per
// filename. CRIU can generate a lot of files (e.g., one per checkpointed application thread).
// We still have a fairly high memory overhead per file of ~150 bytes. See the `restore_mem_usage`
// integration test.

pub trait ImageStore {
    type File: ImageFile;
    fn create(&mut self, filename: &str) -> Result<Self::File>;
    fn close(&mut self, _filename: Box<str>, _file: Self::File) {}
}

pub trait ImageFile {
    fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<()>;
}

pub mod fs {
    use std::fs;
    use super::*;

    pub struct Store<'a> {
        images_dir: &'a Path,
    }

    impl<'a> Store<'a> {
        pub fn new(images_dir: &'a Path) -> Self {
            Self { images_dir }
        }
    }

    impl ImageStore for Store<'_> {
        type File = fs::File;

        fn create(&mut self, filename: &str) -> Result<Self::File> {
            let full_path = &self.images_dir.join(&filename);

            let file = fs::File::create(full_path)
                .with_context(|| format!("Failed to create file {}", full_path.display()))?;

            Ok(file)
        }
    }

    impl ImageFile for fs::File {
        fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<()> {
            shard_pipe.splice_all(self, size)?;
            Ok(())
        }
    }
}

pub mod mem {
    use std::fs;
    use std::io::prelude::*;
    use super::*;
    use crate::mmap_buf::MmapBuf;

    /// CRIU can generate many small files (e.g., core-*.img files), and a few large ones (e.g.,
    /// pages-*.img files). For efficiency, we store small files in a regular `Vec<u8>`. When it
    /// grows bigger than a page size, we switch to a large file implementation that uses mmap
    /// chunks to store its content.
    ///
    /// This is beneficial for avoiding blowing up our memory budget: CRIU copies data from the
    /// image pipe. As CRIU copies data from our memory space, we need to release the memory that
    /// we are holding. The default memory allocator is unpredicable when it comes to using brk(),
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
                        *self = Self::large_from_slice(&chunk);
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
}

pub mod fs_overlay {
    use std::fs;
    use super::*;

    pub struct Store<'a, UnderlyingStore> {
        underlying_store: &'a mut UnderlyingStore,
        overlayed_files: HashMap<Box<str>, fs::File>,
    }

    impl<'a, UnderlyingStore: ImageStore> Store<'a, UnderlyingStore> {
        pub fn new (underlying_store: &'a mut UnderlyingStore) -> Self {
            let overlayed_files = HashMap::new();
            Self { underlying_store, overlayed_files }
        }

        pub fn add_overlay(&mut self, filename: String, file: fs::File) {
            self.overlayed_files.insert(filename.into_boxed_str(), file);
        }
    }

    impl<UnderlyingStore: ImageStore> ImageStore for Store<'_, UnderlyingStore> {
        type File = File<UnderlyingStore::File>;

        fn create(&mut self, filename: &str) -> Result<Self::File> {
            Ok(match self.overlayed_files.remove(filename) {
                Some(file) => File::Overlayed(file),
                None       => File::Underlying(self.underlying_store.create(filename)?),
            })
        }

        fn close(&mut self, filename: Box<str>, output: Self::File) {
            match output {
                File::Overlayed(_) => {},
                File::Underlying(file) => self.underlying_store.close(filename, file),
            }
        }
    }

    pub enum File<UnderlyingFile> {
        Overlayed(fs::File),
        Underlying(UnderlyingFile),
    }

    impl<UnderlyingFile: ImageFile> ImageFile for File<UnderlyingFile> {
        fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<()> {
            match self {
                File::Overlayed(file)  => file.write_all_from_pipe(shard_pipe, size),
                File::Underlying(file) => file.write_all_from_pipe(shard_pipe, size),
            }
        }
    }
}
