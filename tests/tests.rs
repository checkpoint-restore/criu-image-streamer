//  Copyright 2024 Cedana.
//
//  Modifications licensed under the Apache License, Version 2.0.

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

// Unless we are in release mode, allow dead code, unused imports and variables,
// it makes development more enjoyable.
#![cfg_attr(debug_assertions, allow(dead_code, unused_imports, unused_variables))]

#[macro_use]
extern crate anyhow;

mod helpers;

use std::{
    path::PathBuf,
    io::{Read, Write, BufReader},
    cmp::{min, max},
    thread,
};
use cedana_image_streamer::{
    unix_pipe::{UnixPipe, UnixPipeImpl},
    capture::capture,
    extract::{extract, serve},
    util::{KB, MB, PAGE_SIZE},
};
use crate::helpers::{
    criu::Criu,
    util::*,
};
use anyhow::Result;

// Each test belongs in its separate module. They all follow the same workflow, so we made the
// workflow common in the TestImpl trait. Each test modifies some of the behavior of TestImpl to
// test specific part of the workflow.

struct StreamerCheckpointContext {
    progress: BufReader<UnixPipe>,
    capture_thread: thread::JoinHandle<()>,
}

struct CheckpointContext {
    streamer: StreamerCheckpointContext,
    criu: Criu,
}

struct StreamerRestoreContext {
    progress: BufReader<UnixPipe>,
    extract_thread: thread::JoinHandle<()>,
}

struct RestoreContext {
    streamer: StreamerRestoreContext,
    criu: Criu,
}

trait TestImpl {
    fn num_shards(&self) -> usize { 4 }
    fn images_dir(&self) -> PathBuf { PathBuf::from("/tmp/test-cedana-image-streamer") }
    fn capture_ext_files(&mut self) -> Vec<(String, UnixPipe)> { Vec::new() }
    fn extract_ext_files(&mut self) -> Vec<(String, UnixPipe)> { Vec::new() }
    fn serve_image(&mut self) -> bool { true }
    fn has_checkpoint_started(&mut self) -> bool { true } // should be true if send_img_files() has sent a file.

    fn shards(&mut self)-> Vec<(UnixPipe, UnixPipe)> {
        (0..self.num_shards())
            .map(|_| new_pipe())
            .collect()
    }

    fn bootstrap(&mut self) -> Result<(StreamerCheckpointContext, StreamerRestoreContext)> {
        let (capture_progress_r, capture_progress_w) = new_pipe();
        let (extract_progress_r, extract_progress_w) = new_pipe();

        let capture_progress = BufReader::new(capture_progress_r);
        let extract_progress = BufReader::new(extract_progress_r);

        let (shard_pipes_r, shard_pipes_w): (Vec<UnixPipe>, Vec<UnixPipe>) =
            self.shards().drain(..).unzip();

        let capture_thread = {
            let images_dir = self.images_dir();
            let ext_files = self.capture_ext_files();

            thread::spawn(move || {
                capture(&images_dir, capture_progress_w, shard_pipes_w, ext_files)
                    .expect("capture() failed");
            })
        };

        let extract_thread = {
            let images_dir = self.images_dir();
            let ext_files = self.extract_ext_files();
            let serve_image = self.serve_image();

            thread::spawn(move || {
                if serve_image {
                    serve(&images_dir, extract_progress_w, shard_pipes_r, ext_files, vec![])
                        .expect("serve() failed");
                } else {
                    extract(&images_dir, extract_progress_w, shard_pipes_r, ext_files)
                        .expect("extract() failed");
                }
            })
        };

        Ok((
                StreamerCheckpointContext {
                    progress: capture_progress,
                    capture_thread,
                },
                StreamerRestoreContext {
                    progress: extract_progress,
                    extract_thread,
                }
        ))
    }

    fn criu_checkpoint_connect(&mut self, mut checkpoint: StreamerCheckpointContext)
        -> Result<CheckpointContext>
    {
        // Wait for CRIU socket for checkpointing to be ready
        assert_eq!(read_line(&mut checkpoint.progress)?, "socket-init");
        let criu = Criu::connect(self.images_dir().join("streamer-capture.sock"))?;
        Ok(CheckpointContext { streamer: checkpoint, criu })
    }

    fn send_img_files(&mut self, _checkpoint: &mut CheckpointContext) -> Result<()> {
        Ok(())
    }

    fn read_progress_checkpoint_started(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
        if self.has_checkpoint_started() {
            assert_eq!(read_line(&mut checkpoint.streamer.progress)?, "checkpoint-start");
        }
        Ok(())
    }

    fn finish_checkpoint(&mut self, mut checkpoint: CheckpointContext) -> Result<Stats> {
        checkpoint.criu.finish()?;
        let stats: Stats = read_stats(&mut checkpoint.streamer.progress)?;
        checkpoint.streamer.capture_thread.join().unwrap();
        Ok(stats)
    }

    fn after_finish_checkpoint(&mut self, _checkpoint_stats: &Stats) -> Result<()> {
        Ok(())
    }

    fn finish_image_extraction(&mut self, restore: &mut StreamerRestoreContext) -> Result<Stats> {
        Ok(read_stats(&mut restore.progress)?)
    }

    fn after_finish_image_extraction(&mut self, _restore_stats: &Stats) -> Result<()> {
        Ok(())
    }

    fn criu_restore_connect(&mut self, mut restore: StreamerRestoreContext)
        -> Result<RestoreContext>
    {
        // The image can be served now. Wait for the CRIU socket to be ready.
        assert_eq!(read_line(&mut restore.progress)?, "socket-init");
        let criu = Criu::connect(self.images_dir().join("streamer-serve.sock"))?;
        Ok(RestoreContext { streamer: restore, criu })
    }

    fn recv_img_files(&mut self, _restore: &mut RestoreContext) -> Result<()> {
        Ok(())
    }

    fn finish_restore(&mut self, mut restore: RestoreContext) -> Result<()> {
        restore.criu.finish()?;
        restore.streamer.extract_thread.join().unwrap();
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let (checkpoint, mut restore) = self.bootstrap()?;

        let mut checkpoint = self.criu_checkpoint_connect(checkpoint)?;
        self.send_img_files(&mut checkpoint)?;
        self.read_progress_checkpoint_started(&mut checkpoint)?;
        let stats = self.finish_checkpoint(checkpoint)?;
        self.after_finish_checkpoint(&stats)?;

        let stats = self.finish_image_extraction(&mut restore)?;
        self.after_finish_image_extraction(&stats)?;

        if self.serve_image() {
            let mut restore = self.criu_restore_connect(restore)?;
            self.recv_img_files(&mut restore)?;
            self.finish_restore(restore)?;
        }

        Ok(())
    }
}

mod basic {
    use super::*;

    struct Test;

    impl Test {
        fn new() -> Self { Self }
    }

    impl TestImpl for Test {
        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            checkpoint.criu.write_img_file("file.img")?
                .write_all("hello world".as_bytes())?;

            Ok(())
        }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            let buf = restore.criu.read_img_file_into_vec("file.img")?;
            assert_eq!(buf, "hello world".as_bytes(), "File data content mismatch");
            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod missing_files {
    use super::*;

    struct Test;

    impl Test {
        fn new() -> Self { Self }
    }

    impl TestImpl for Test {
        fn has_checkpoint_started(&mut self) -> bool { false }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            let file = restore.criu.maybe_read_img_file("no-file.img")?;
            assert!(file.is_none(), "File exists but shouldn't");
            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod ext_files {
    use super::*;

    const TEST_DATA1: &str = "ext file1 data";
    const TEST_DATA2: &str = "ext file2 data";

    struct Test {
        send_ext_pipe1: Option<UnixPipe>,
        send_ext_pipe2: Option<UnixPipe>,

        recv_ext_pipe1: Option<UnixPipe>,
        recv_ext_pipe2: Option<UnixPipe>,
    }

    impl Test {
        fn new() -> Self {
            Self {
                send_ext_pipe1: None, send_ext_pipe2: None,
                recv_ext_pipe1: None, recv_ext_pipe2: None,
            }
        }
    }

    impl TestImpl for Test {
        fn has_checkpoint_started(&mut self) -> bool { false }

        fn capture_ext_files(&mut self) -> Vec<(String, UnixPipe)> {
            let (pipe1_r, pipe1_w) = new_pipe();
            let (pipe2_r, pipe2_w) = new_pipe();

            self.send_ext_pipe1 = Some(pipe1_w);
            self.send_ext_pipe2 = Some(pipe2_w);

            vec![("file1.ext".to_string(), pipe1_r),
                 ("file2.ext".to_string(), pipe2_r)]
        }

        fn extract_ext_files(&mut self) -> Vec<(String, UnixPipe)> {
            let (pipe1_r, pipe1_w) = new_pipe();
            let (pipe2_r, pipe2_w) = new_pipe();

            self.recv_ext_pipe1 = Some(pipe1_r);
            self.recv_ext_pipe2 = Some(pipe2_r);

            vec![("file1.ext".to_string(), pipe1_w),
                 ("file2.ext".to_string(), pipe2_w)]
        }

        fn send_img_files(&mut self, _: &mut CheckpointContext) -> Result<()> {
            self.send_ext_pipe1.take().unwrap().write_all(TEST_DATA1.as_bytes())?;
            self.send_ext_pipe2.take().unwrap().write_all(TEST_DATA2.as_bytes())?;
            Ok(())
        }

        fn recv_img_files(&mut self, _: &mut RestoreContext) -> Result<()> {
            let mut buf = Vec::new();
            self.recv_ext_pipe1.take().unwrap().read_to_end(&mut buf)?;
            assert_eq!(buf, TEST_DATA1.as_bytes());

            let mut buf = Vec::new();
            self.recv_ext_pipe2.take().unwrap().read_to_end(&mut buf)?;
            assert_eq!(buf, TEST_DATA2.as_bytes());

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod load_balancing {
    use super::*;

    // This test simulates a capture with 4 shards, where the first one is being rate limited
    // at 1MB/s. We attempt to capture a 40MB image. The choke shard should receive little
    // data compared to the other shards.
    //
    // NOTE Load balancing works only when using pipes of capacity greater than 2*PAGE_SIZE.
    // We skip that test if we don't have enough pipe capacity.
    // The Rust test runner lacks the ability to skip a test at runtime, so we improvised a bit.

    const CHOKE_RATE_PER_MILLI: usize = 1*KB; // 1MB/sec
    const CHOKE_SHARD_INDEX: usize = 0;

    struct Test {
        file: Vec<u8>,
        shard_threads: Option<Vec<thread::JoinHandle<Result<()>>>>,
        skip_test: bool,
    }

    impl Test {
        fn new() -> Self {
            Self {
                // Large enough to fill the buffer of the choked pipe
                file: get_rand_vec(40*MB),
                shard_threads: None,
                skip_test: false,
            }
        }
    }

    impl TestImpl for Test {
        fn shards(&mut self)-> Vec<(UnixPipe, UnixPipe)> {
            let (shards, shard_threads) = (0..4).map(|i| {
                let (mut capture_shard_r, capture_shard_w) = new_pipe();
                let (extract_shard_r, mut extract_shard_w) = new_pipe();
                let shard = (extract_shard_r, capture_shard_w);

                let shard_thread = thread::spawn(move || {
                    let mut buf = Vec::new();
                    if i == CHOKE_SHARD_INDEX {
                        read_to_end_rate_limited(&mut capture_shard_r, &mut buf, CHOKE_RATE_PER_MILLI)?;
                    } else {
                        capture_shard_r.read_to_end(&mut buf)?;
                    }
                    extract_shard_w.write_all(&buf)?;
                    Ok(())
                });

                (shard, shard_thread)
            }).unzip();

            self.shard_threads = Some(shard_threads);

            // Check if we have enough capacity for pipes
            let mut shards: Vec<(UnixPipe, UnixPipe)> = shards;
            for (shard_r, _) in &mut shards {
                if shard_r.set_capacity(2*(*PAGE_SIZE) as i32).is_err() {
                    self.skip_test = true;
                    eprintln!("WARN Skipping load_balancing test due to insufficient pipe capacity. \
                               This test needs at least 2 page capacity for shard pipes");
                    break;
                }
            }

            shards
        }

        fn has_checkpoint_started(&mut self) -> bool { !self.skip_test }

        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            if self.skip_test {
                return Ok(());
            }

            checkpoint.criu.write_img_file("file.img")?
                .write_all(&self.file)?;

            Ok(())
        }

        fn after_finish_checkpoint(&mut self, checkpoint_stats: &Stats) -> Result<()> {
            if self.skip_test {
                return Ok(());
            }

            self.shard_threads.take().unwrap()
                .drain(..).map(|t| t.join().unwrap())
                .collect::<Result<_>>()?;

            eprintln!("Shard sizes: {:?} KB", checkpoint_stats.shards.iter()
                      .map(|s| s.size/KB as u64).collect::<Vec<_>>());

            for (i, shard_stats) in checkpoint_stats.shards.iter().enumerate() {
                // Using 2MB threadshold as the choked shard has to be bigger than 1MB (SHARD_PIPE_CAPACITY),
                // but not much more.
                if i == CHOKE_SHARD_INDEX {
                    assert!(shard_stats.size < 2*MB as u64,
                            "Choked shard received too much data: {} KB", shard_stats.size/KB as u64);
                } else {
                    assert!(shard_stats.size > 2*MB as u64,
                            "Normal shard received too little data: {} KB", shard_stats.size/KB as u64);
                }
            }

            Ok(())
        }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            if self.skip_test {
                return Ok(());
            }

            let buf = restore.criu.read_img_file_into_vec("file.img")?;
            assert!(buf == self.file);

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod restore_mem_usage {
    use super::*;

    // We test one large file, and many small ones, simulating a fair CRIU workload.
    // There are two things to test:
    // 1) The in-memory store should have low overhead. 200 bytes per file is not exactly good, but
    //    it's good enough (measured at 129 bytes). I welcome suggestion to make this better.
    // 2) The in-memory store should free its memory as its transferring a file to CRIU
    //    It shouldn't be bigger than image_store::mem::MAX_LARGE_CHUNK_SIZE (10MB). We add a bit
    //    for slack.

    const BIG_FILE_SIZE: usize = 105*MB;
    const SMALL_FILE_SIZE: usize = 10;
    const NUM_SMALL_FILES: usize = 100_000;
    const TOLERABLE_PER_FILE_OVERHEAD: isize = 200 as isize;
    const TOLERABLE_CRIU_RECEIVE_OVERHEAD: isize = 12*MB as isize;

    struct Test {
        start_mem_size: Option<usize>
    }

    impl Test {
        fn new() -> Self {
            Self { start_mem_size: None }
        }
    }

    impl TestImpl for Test {
        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            self.start_mem_size = Some(get_resident_mem_size());

            let small = get_filled_vec(SMALL_FILE_SIZE, 1);

            for i in 0..NUM_SMALL_FILES {
                let filename = format!("small-{}.img", i);
                checkpoint.criu.write_img_file(&filename)?.write_all(&small)?;
            }

            // Writing the big file in small chunks, to prevent blowing up memory with a large
            // vector that may not get freed.
            let mut big_file_pipe = checkpoint.criu.write_img_file("big.img")?;
            let buf = get_filled_vec(1*KB, 1);
            for _ in 0..(BIG_FILE_SIZE/buf.len()) {
                big_file_pipe.write_all(&buf)?;
            }

            Ok(())
        }

        fn after_finish_image_extraction(&mut self, _restore_stats: &Stats) -> Result<()> {
            let extraction_use = get_resident_mem_size() as isize - self.start_mem_size.unwrap() as isize;
            let overhead = extraction_use  as isize - (BIG_FILE_SIZE + NUM_SMALL_FILES * SMALL_FILE_SIZE) as isize;
            let overhead_per_file = overhead / (1 + NUM_SMALL_FILES) as isize;

            assert!(overhead_per_file < TOLERABLE_PER_FILE_OVERHEAD,
                    "In-memory image store shows too much memory overhead per file: {} bytes", overhead_per_file);
            eprintln!("Per file overhead: {} bytes", overhead_per_file);
            Ok(())
        }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            let start_recv_mem_usage = get_resident_mem_size();

            let mut big_file = Vec::with_capacity(BIG_FILE_SIZE);
            let big_file_pipe = &restore.criu.read_img_file("big.img")?;
            let mut max_overhead = 0;
            loop {
                let count = big_file_pipe.take(10*KB as u64).read_to_end(&mut big_file)?;

                let delta_recv_mem_usage = get_resident_mem_size() as isize - start_recv_mem_usage as isize;
                max_overhead = max(max_overhead, delta_recv_mem_usage);

                if count == 0 {
                    break; // EOF
                }
            }

            assert!(max_overhead < TOLERABLE_CRIU_RECEIVE_OVERHEAD,
                "Memory budget exceeded while CRIU reads a large file: {} MB", max_overhead/MB as isize);
            eprintln!("Large file overhead: {} MB", max_overhead/MB as isize);

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod stress {
    use super::*;
    use crossbeam_utils::thread;
    use std::sync::Mutex;

    const NUM_THREADS: usize = 5;
    const NUM_SMALL_FILES: usize = 10000;
    const NUM_MEDIUM_FILES: usize = 1000;
    const NUM_MEDIUM_CHUNKED_FILES: usize = 1000;
    const NUM_LARGE_FILES: usize = 2;

    const SMALL_FILE_SIZE: usize = 10;
    const MEDIUM_FILE_SIZE: usize = 10*KB;
    const LARGE_FILE_SIZE: usize = 25*MB; // 25MB to have a few mmap buffers
    const CHUNK_SIZE: usize = 10;

    struct Test {
        small_file: Vec<u8>,
        medium_file: Vec<u8>,
        large_file: Vec<u8>,
    }

    impl Test {
        fn new() -> Self {
            let small_file  = get_rand_vec(SMALL_FILE_SIZE);
            let medium_file = get_rand_vec(MEDIUM_FILE_SIZE);
            let large_file  = get_rand_vec(LARGE_FILE_SIZE);

            Self { small_file, medium_file, large_file }
        }
    }

    impl TestImpl for Test {
        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            let checkpoint = Mutex::new(checkpoint);

            let write_img_file = |name: &str, data: &Vec<u8>| -> Result<()> {
                let mut pipe = checkpoint.lock().unwrap().criu.write_img_file(name)?;
                pipe.write_all(data)?;
                Ok(())
            };

            let write_img_file_chunked = |name: &str, data: &Vec<u8>| -> Result<()> {
                let mut pipe = checkpoint.lock().unwrap().criu.write_img_file(name)?;

                let mut offset = 0;
                while offset < data.len() {
                    let to_write = min(data.len() - offset, CHUNK_SIZE);
                    pipe.write_all(&data[offset..offset+to_write])?;
                    offset += to_write;
                }

                Ok(())
            };

            let write_files = |i: usize| {
                for j in 0..NUM_MEDIUM_FILES {
                    write_img_file(&format!("medium-{}-{}.img", i, j), &self.medium_file).unwrap();
                }
                for j in 0..NUM_LARGE_FILES {
                    write_img_file(&format!("large-{}-{}.img", i, j), &self.large_file).unwrap();
                }
                for j in 0..NUM_SMALL_FILES {
                    write_img_file(&format!("small-{}-{}.img", i, j), &self.small_file).unwrap();
                }
                for j in 0..NUM_MEDIUM_CHUNKED_FILES {
                    write_img_file_chunked(&format!("mediumc-{}-{}.img", i, j), &self.medium_file).unwrap();
                }
            };

            // Write without threads first. Should be easier
            write_files(0);

            thread::scope(|s| {
                for i in 0..NUM_THREADS {
                    s.spawn(move |_| write_files(i+1));
                }
            }).unwrap();

            Ok(())
        }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            let mut read_img_file = |name: &str| -> Result<Vec<u8>> {
                restore.criu.read_img_file_into_vec(name)
            };

            for i in 0..NUM_THREADS+1 {
                for j in 0..NUM_MEDIUM_FILES {
                    assert!(read_img_file(&format!("medium-{}-{}.img", i, j))? == self.medium_file);
                }
                for j in 0..NUM_LARGE_FILES {
                    assert!(read_img_file(&format!("large-{}-{}.img", i, j))? == self.large_file);
                }
                for j in 0..NUM_SMALL_FILES {
                    assert!(read_img_file(&format!("small-{}-{}.img", i, j))? == self.small_file);
                }
                for j in 0..NUM_MEDIUM_CHUNKED_FILES {
                    assert!(read_img_file(&format!("mediumc-{}-{}.img", i, j))? == self.medium_file);
                }
            }

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod splice_bug {
    // The 4.14.67 kernel can corrupt data with splice(), but the 4.14.121 has that bug fixed.
    // This test makes it obvious. It's a subset of the stress test above, but much faster.
    // Maybe the fix is https://github.com/torvalds/linux/commit/1bdc347
    // We should be careful not to use such buggy kernel.
    use super::*;
    use std::sync::Mutex;

    const NUM_MEDIUM_CHUNKED_FILES: usize = 100;
    const MEDIUM_FILE_SIZE: usize = 10*KB;
    const CHUNK_SIZE: usize = 10;

    struct Test {
        medium_file: Vec<u8>,
    }

    impl Test {
        fn new() -> Self {
            let medium_file = get_rand_vec(MEDIUM_FILE_SIZE);
            Self { medium_file }
        }
    }

    impl TestImpl for Test {
        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            let checkpoint = Mutex::new(checkpoint);

            let write_img_file_chunked = |name: &str, data: &Vec<u8>| -> Result<()> {
                let mut pipe = checkpoint.lock().unwrap().criu.write_img_file(name)?;

                let mut offset = 0;
                while offset < data.len() {
                    let to_write = min(data.len() - offset, CHUNK_SIZE);
                    pipe.write_all(&data[offset..offset+to_write])?;
                    offset += to_write;
                }

                Ok(())
            };

            let write_files = |i: usize| {
                for j in 0..NUM_MEDIUM_CHUNKED_FILES {
                    write_img_file_chunked(&format!("mediumc-{}-{}.img", i, j), &self.medium_file).unwrap();
                }
            };

            write_files(0);

            Ok(())
        }

        fn recv_img_files(&mut self, restore: &mut RestoreContext) -> Result<()> {
            let mut read_img_file = |name: &str| -> Result<Vec<u8>> {
                restore.criu.read_img_file_into_vec(name)
            };

            let i = 0;
            for j in 0..NUM_MEDIUM_CHUNKED_FILES {
                assert!(read_img_file(&format!("mediumc-{}-{}.img", i, j))? == self.medium_file);
            }

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}

mod extract_to_disk {
    use super::*;
    use std::fs::File;

    struct Test {
        small_file: Vec<u8>,
        medium_file: Vec<u8>,
    }

    impl Test {
        fn new() -> Self {
            let small_file = get_rand_vec(1*KB);
            let medium_file = get_rand_vec(100*KB);
            Self { small_file, medium_file }
        }
    }

    impl TestImpl for Test {
        fn serve_image(&mut self) -> bool { false }

        fn send_img_files(&mut self, checkpoint: &mut CheckpointContext) -> Result<()> {
            checkpoint.criu.write_img_file("small.img")?
                .write_all(&self.small_file)?;
            checkpoint.criu.write_img_file("medium.img")?
                .write_all(&self.medium_file)?;
            Ok(())
        }

        fn after_finish_image_extraction(&mut self, _restore_stats: &Stats) -> Result<()> {
            let read_img_file = |name: &str| -> Result<Vec<u8>> {
                let mut buf = Vec::new();
                let mut small_file = File::open(self.images_dir().join(name))?;
                small_file.read_to_end(&mut buf)?;
                Ok(buf)
            };

            assert!(read_img_file("small.img")? == self.small_file);
            assert!(read_img_file("medium.img")? == self.medium_file);

            Ok(())
        }
    }

    #[test]
    fn test() -> Result<()> {
        Test::new().run()
    }
}
