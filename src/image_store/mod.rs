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

pub mod fs_overlay;
pub mod fs;
pub mod mem;

use anyhow::Result;
use crate::unix_pipe::UnixPipe;

// The `ImageStore` is only used during image extraction.
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
    /// `create()` returns a `File`, which can be written to.
    fn create(&mut self, filename: &str) -> Result<Self::File>;
    /// `insert()` takes ownership of a previously created file, and insert it
    /// in the image store.
    fn insert(&mut self, filename: impl Into<Box<str>>, file: Self::File);
}

pub trait ImageFile {
    fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<()>;
}
