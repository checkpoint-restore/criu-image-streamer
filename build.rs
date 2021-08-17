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

use std::path::PathBuf;

fn get_proto_files(dir: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir).expect("read_dir failed")
      .map(|file| file.unwrap().path())
      .filter(|path| path.extension() == Some(std::ffi::OsStr::new("proto")))
      .collect()
}

fn main() {
    prost_build::compile_protos(&get_proto_files("proto/"),
                                &[PathBuf::from("proto/")])
        .expect("Failed to generate protobuf wrappers");

    prost_build::compile_protos(&get_proto_files("proto/criu"),
                                &[PathBuf::from("proto/criu")])
        .expect("Failed to generate protobuf wrappers");
}
