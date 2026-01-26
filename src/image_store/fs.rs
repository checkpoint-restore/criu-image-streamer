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
    path::Path,
};
use crate::{
    unix_pipe::{UnixPipe, UnixPipeImpl},
};

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
        let full_path = &self.images_dir.join(filename);

        let file = fs::File::create(full_path)
            .with_context(|| format!("Failed to create file {}", full_path.display()))?;

        Ok(file)
    }

    fn insert(&mut self, _filename: impl Into<Box<str>>, _file: Self::File) {
        // Nothing to do, the file is on disk already.
    }
}

impl ImageFile for fs::File {
    fn write_all_from_pipe(&mut self, shard_pipe: &mut UnixPipe, size: usize) -> Result<()> {
        shard_pipe.splice_all(self, size)?;
        Ok(())
    }
}
