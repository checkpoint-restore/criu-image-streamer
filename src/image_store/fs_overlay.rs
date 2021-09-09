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
use anyhow::Result;
use std::{
    fs,
    collections::HashMap,
};
use crate::{
    unix_pipe::UnixPipe,
};

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

    fn insert(&mut self, filename: impl Into<Box<str>>, output: Self::File) {
        match output {
            File::Overlayed(_) => {},
            File::Underlying(file) => self.underlying_store.insert(filename, file),
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
