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

use std::{
    collections::{BinaryHeap, HashMap, HashSet},
    os::unix::io::AsRawFd,
    time::Instant,
    path::Path,
};
use crate::{
    endpoint_connection::EndpointListener,
    unix_pipe::{UnixPipe, UnixPipeImpl},
    util::*,
    image,
    image::marker,
    impl_ord_by,
    image_store,
    image_store::{ImageStore, ImageFile},
    prnt,
};
use nix::poll::{poll, PollFd, PollFlags};
use anyhow::{Result, Context};

// The serialized image is received via multiple data streams (`Shard`). The data streams are
// comprised of markers followed by an optional data payload. The format of the markers is
// described in ../proto/image.proto.
// Each marker has a sequence number providing a reassembly order (produced in capture.rs).
// At any point in time, a given shard has the marker that should be processed next.
// We represent this with a PendingMarker. Reassembling the stream of markers provides the
// original image files.
//
// When extracting the image, we either store the image files in memory, or write them on disk.
// The former is useful when streaming to CRIU directly, the latter is useful to extract an image
// on disk.
//
// Streaming to CRIU is done by buffering the entire image in memory, and let CRIU consume it.
// XXX Performance isn't that great due to the memory copy in our address space. To improve
// performance, we could splice() shard pipe data to CRIU directly. This is difficult as CRIU
// doesn't read the image files in the same order as they are produced. For example, inventory.img
// is written last in the image, but is read first. One way to go around this issue is to reserve
// a shard during capture for all small image files (pretty much all except pages, ghost files, and
// fs.tar). In addition, we might have to rewrite some part of CRIU to restore these large files in
// the same order as they were produced. It might be difficult to preserve this guarantee forever,
// so it would be wise to keep our in-memory buffering implementation anyways.

/// We are not doing zero-copy transfers to CRIU (yet), we have to be mindful of CPU caches.
/// If we were doing shard to CRIU splices, we could bump the capacity to 4MB.
#[allow(clippy::identity_op)]
const CPU_PIPE_DESIRED_CAPACITY: i32 = 1*MB as i32;
const GPU_PIPE_DESIRED_CAPACITY: i32 = 16*MB as i32;


/// Data comes in a stream of chunks, which can be as large as 2MB (from capture.rs).
/// We use 8MB to have four chunks in to avoid stalling the shards.
/// CPU cache interference experimentally seems minimal, larger sizes help performance.
const SHARD_PIPE_DESIRED_CAPACITY: i32 = 8*MB as i32;

struct Shard {
    pipe: UnixPipe,
    transfer_duration_millis: u64,
    bytes_read: u64,
}

impl Shard {
    fn new(mut pipe: UnixPipe) -> Self {
        // Try setting the pipe capacity. Failing is okay, it's just for better performance.
        let _ = pipe.set_capacity(SHARD_PIPE_DESIRED_CAPACITY);
        Self { pipe, bytes_read: 0, transfer_duration_millis: 0 }
    }
}

struct PendingMarker<'a> {
    marker: image::Marker,
    shard: &'a mut Shard,
}

// This gives ordering of the PendingMarker for the BinaryHeap (max-heap). Lowest `seq` comes first,
// hence the `reverse()`. Note that sequence numbers are unique, giving us a total order.
impl_ord_by!(PendingMarker<'a>, |a: &Self, b: &Self| a.marker.seq.cmp(&b.marker.seq).reverse());

struct ImageDeserializer<'a, ImgStore: ImageStore> {
    // Shards are located in three different collections:
    // 1) `shards` stores shards that may not be readable yet. `poll()` is used to determine when a
    //    shard is readable, at which point it is moved to the `readable_shard` vec.
    // 2) `readable_shards` stores shards that are definitely readable. When reading a marker from
    //    a shard, its sequence number is examined and the pair (marker, shard) denoted by the type
    //    `PendingMarker` is placed into the `pending_markers` binary heap.
    // 3) `pending_markers` stores a sorted collection of these pending markers by sequence number.
    //    Once a marker matches the sequence number that we need (stored in the `seq` field), it is
    //    processed with its associated shard. Once processed, the shard goes back in the shards
    //    vec, and the cycle continues.
    shards: Vec<&'a mut Shard>,
    readable_shards: Vec<&'a mut Shard>,
    pending_markers: BinaryHeap<PendingMarker<'a>>,
    seq: u64,

    // The following fields relate to the output.
    // When receiving a `Filename(filename)` marker, the `img_files` map is examined to see if we
    // have an image file corresponding to that filename. If not found, a new image file is created
    // via the `img_store`. The image file is then placed into `current_img_file`, which becomes the
    // default destination for incoming data via `FileData` markers.
    //
    // We use a `Box<str>` instead of `String` for filenames to reduce memory usage by 8 bytes per
    // filenames (`str` are not resizable, `Strings` are, so they need to carry additional information).
    img_store: &'a mut ImgStore,
    img_files: HashMap<Box<str>, ImgStore::File>,
    current_img_file: Option<(Box<str>, ImgStore::File)>,

    // `start_time` is used for stats, image_eof is used for safety checks.
    start_time: Instant,
    image_eof: bool,
}

impl<'a, ImgStore: ImageStore> ImageDeserializer<'a, ImgStore> {
    pub fn new(img_store: &'a mut ImgStore, shards: &'a mut [Shard]) -> Self {
        let num_shards = shards.len();
        Self {
            shards: shards.iter_mut().collect(),
            readable_shards: Vec::with_capacity(num_shards),
            pending_markers: BinaryHeap::with_capacity(num_shards),
            seq: 0,
            img_store,
            img_files: HashMap::new(),
            current_img_file: None,
            start_time: Instant::now(),
            image_eof: false,
        }
    }

    fn mark_image_eof(&mut self) -> Result<()> {
        ensure!(self.img_files.is_empty() && self.pending_markers.is_empty(),
                "Image EOF marker came unexpectedly");

        self.image_eof = true;
        Ok(())
    }

    fn select_img_file(&mut self, filename: Box<str>) -> Result<()> {
        // First, put the current image file back in the hashmap.
        // This avoids creating the same image file twice.
        if let Some((filename, output)) = self.current_img_file.take() {
            self.img_files.insert(filename, output);
        }

        // Then, look for an image file in the hashmap with the corresponding filename.
        // If not found, create a new image file.
        let (filename, img_file) = match self.img_files.remove_entry(&filename) {
            Some((filename, img_file)) => (filename, img_file),
            None => {
                let img_file = self.img_store.create(&filename)?;
                (filename, img_file)
            }
        };

        self.current_img_file = Some((filename, img_file));
        Ok(())
    }

    fn process_marker(&mut self, marker: image::Marker, shard: &mut Shard) -> Result<()> {
        use marker::Body::*;

        match marker.body {
            Some(Filename(filename)) => {
                self.select_img_file(filename.into_boxed_str())?;
            }
            Some(FileData(size)) => {
                let (_filename, img_file) = self.current_img_file.as_mut()
                    .ok_or_else(|| anyhow!("Unexpected FileData marker"))?;
                img_file.write_all_from_pipe(&mut shard.pipe, size as usize)?;
                shard.bytes_read += size as u64;
            }
            Some(FileEof(true)) => {
                let (filename, img_file) = self.current_img_file.take()
                    .ok_or_else(|| anyhow!("Unexpected FileEof marker"))?;
                self.img_store.insert(filename, img_file);
            }
            Some(ImageEof(true)) => {
                self.mark_image_eof()?;
            }
            _ => bail!("Malformed image marker"),
        }

        Ok(())
    }

    fn get_next_in_order_marker(&mut self) -> Option<PendingMarker<'a>> {
        if let Some(pmarker) = self.pending_markers.peek() {
            if pmarker.marker.seq == self.seq {
                return self.pending_markers.pop();
            }
        }
        None
    }

    fn process_pending_markers(&mut self) -> Result<()> {
        while let Some(PendingMarker { marker, mut shard }) = self.get_next_in_order_marker() {
            self.process_marker(marker, &mut shard)?;
            self.seq += 1;
            self.shards.push(shard);
        }
        Ok(())
    }

    fn mark_shard_eof(&self, shard: &mut Shard) {
        shard.transfer_duration_millis = self.start_time.elapsed().as_millis().try_into().unwrap();
    }

    fn drain_shard(&mut self, shard: &'a mut Shard) -> Result<()> {
        match pb_read_next(&mut shard.pipe)? {
            None => {
                // EOF of that shard is reached
                self.mark_shard_eof(shard);
            }
            Some((marker, marker_size)) => {
                ensure!(!self.image_eof, "Unexpected data after image EOF");
                shard.bytes_read += marker_size as u64;
                self.pending_markers.push(PendingMarker { marker, shard });
                self.process_pending_markers()?;
            }
        }
        Ok(())
    }

    fn get_next_readable_shard(&mut self) -> Result<Option<&'a mut Shard>> {
        // heavily investigate here TODO
        // If we just return `self.shard.pop()`, we may deadlock if shard pipes are not independent
        // from each other.
        // This scenario only happens when the capture shards are directly connected to the extract
        // shards, useful when doing live migrations. The deadlock may happen when the capture
        // serializer attempts to push a large chunk down a shard, and blocks because the shard is
        // full. Meanwhile, the extract reader could be blocking on reading from an empty pipe
        // shard in `pb_read_next()`.
        // To tolerate these workloads, we need to read from shards that are guaranteed to have
        // available data.
        // We use poll() instead of epoll() because we need to ignore the shards that are in the
        // list of pending markers, and we are not doing async reads to do edge triggers.
        if self.readable_shards.is_empty() {
            if self.shards.len() <= 0 {
                // If we have no shard to read from, we'll return None.
                // If we have a single shard to read from, there no need to block in poll()
                // We return immediately with that shard, even if it is not readable yet as it
                // won't introduce a deadlock with the capture side.
                prnt!("self.shards.len ({}) <= 1, returning Ok(self.shards.pop())", self.shards.len());
                return Ok(self.shards.pop());
            }

            let mut poll_fds: Vec<PollFd> = self.shards.iter()
                .map(|shard| PollFd::new(shard.pipe.as_raw_fd(), PollFlags::POLLIN))
                .collect();

            let timeout = -1;
            let n = poll(&mut poll_fds, timeout)?;
            assert!(n > 0); // There should be at least one fd ready.

            // We could use drain_filter() instead of the mem::replace dance, but we'll probably
            // have to use zip(), which complicates the code.
            let shards = {
                let capacity = self.shards.capacity();
                std::mem::replace(&mut self.shards, Vec::with_capacity(capacity))
            };
            for (shard, poll_fd) in shards.into_iter().zip(poll_fds) {
                // We can unwrap() safely. It is fair to assume that the kernel returned valid bits
                // in `revents`.
                if !poll_fd.revents().unwrap().is_empty() {
                    self.readable_shards.push(shard);
                } else {
                    self.shards.push(shard);
                }
            }
        }

        Ok(self.readable_shards.pop())
    }

    /// Returns successfully when the image has been fully deserialized. This is our main loop.
    pub fn drain_all(&mut self) -> Result<()> {
        while let Some(shard) = self.get_next_readable_shard()? {
            // prnt!("draining a shard");
            self.drain_shard(shard)?;
        }
        ensure!(self.image_eof, "No shards to read from");
        Ok(())
    }
}

/// `serve_img()` serves the in-memory image store to CRIU.
fn serve_img(
    mem_store: &mut image_store::mem::Store,
    ced_listener: EndpointListener,
    gpu_listener: Option<EndpointListener>,
    criu_listener: EndpointListener,
) -> Result<()>
{
    let mut ced = ced_listener.into_accept()?;

    prnt!("connected to daemon");
    let mut filenames_of_sent_files = HashSet::new();
    if let Some(filename) = ced.read_next_file_request()? {
        match mem_store.remove(&filename) {
            Some(memory_file) => {
                prnt!("daemon filename: {}", &filename);
                filenames_of_sent_files.insert(filename.clone());
                ced.send_file_reply(true)?; // true means that the file exists.
                let mut pipe = ced.recv_pipe()?;
                // Try setting the pipe capacity. Failing is okay.
                let _ = pipe.set_capacity(CPU_PIPE_DESIRED_CAPACITY);
                memory_file.drain(&mut pipe)
                    .with_context(|| format!("while serving file {}", &filename))?;
            }
            None => {
                // If we keep the image file in our process, CRIU will also
                // have a copy of the image file. This uses x2 the memory for an image
                // file. For large files like memory pages, we could very much go over
                // the machine memory capacity.
                ensure!(!filenames_of_sent_files.contains(&filename),
                    "Daemon is requesting the image file `{}` multiple times. \
                    This is not allowed to keep the memory usage low", &filename);
                ced.send_file_reply(false)?;
            }
        }
    }
    prnt!("finished listening to daemon");

    match gpu_listener {
        Some(g) => {
            let mut gpu = g.into_accept()?;
            prnt!("connected to gpu");
            while let Some(filename_prefix) = gpu.read_next_file_request()? {
                match mem_store.remove_by_prefix(&filename_prefix) {
                    Some((filename, memory_file)) => {
                        prnt!("gpu filename: {}", &filename);
                        filenames_of_sent_files.insert(filename.to_string().clone());
                        gpu.send_file_reply(true)?;
                        let mut pipe = gpu.recv_pipe()?;
                        // Try setting the pipe capacity. Failing is okay.
                        let _ = pipe.set_capacity(GPU_PIPE_DESIRED_CAPACITY);
                        memory_file.drain(&mut pipe)
                            .with_context(|| format!("while serving file_prefix {}", &filename_prefix))?;
                    }
                    None => {
                        // If we keep the image file in our process, CRIU will also
                        // have a copy of the image file. This uses x2 the memory for an image
                        // file. For large files like memory pages, we could very much go over
                        // the machine memory capacity.
                        ensure!(!filenames_of_sent_files.contains(&filename_prefix),
                            "cedana-gpu-controller is requesting the image file prefix `{}` multiple times. \
                            This is not allowed to keep the memory usage low", &filename_prefix);
                        gpu.send_file_reply(false)?;
                    }
                }
            }
            prnt!("finished listening to gpu");
        },
        None => { prnt!("not using gpu"); },
    }

    let mut criu = criu_listener.into_accept()?;
    prnt!("connected to criu");
    // XXX Currently, CRIU reads image files sequentially. If it were to read files in an
    // interleaved fashion, we would have to use the Poller to avoid deadlocks.

    while let Some(filename) = criu.read_next_file_request()? {
        match mem_store.remove(&filename) {
            Some(memory_file) => {
                prnt!("criu filename: {}", &filename);
                filenames_of_sent_files.insert(filename.clone());
                criu.send_file_reply(true)?; // true means that the file exists.
                let mut pipe = criu.recv_pipe()?;
                // Try setting the pipe capacity. Failing is okay.
                let _ = pipe.set_capacity(CPU_PIPE_DESIRED_CAPACITY);
                memory_file.drain(&mut pipe)
                    .with_context(|| format!("while serving file {}", &filename))?;
            }
            None => {
                // If we keep the image file in our process, CRIU will also
                // have a copy of the image file. This uses x2 the memory for an image
                // file. For large files like memory pages, we could very much go over
                // the machine memory capacity.
                ensure!(!filenames_of_sent_files.contains(&filename),
                    "CRIU is requesting the image file `{}` multiple times. \
                    This is not allowed to keep the memory usage low", &filename);
                criu.send_file_reply(false)?;
            }
        }
    }
    prnt!("finished listening to criu");

    Ok(())
}

fn drain_shards_into_img_store<Store: ImageStore>(
    img_store: &mut Store,
    shard_pipes: Vec<UnixPipe>,
) -> Result<()>
{
    let mut shards: Vec<Shard> = shard_pipes.into_iter().map(Shard::new).collect();

    let mut overlayed_img_store = image_store::fs_overlay::Store::new(img_store);
    let mut img_deserializer = ImageDeserializer::new(&mut overlayed_img_store, &mut shards);
    img_deserializer.drain_all()?;

    Ok(())
}

/// Description of the arguments can be found in main.rs
pub fn serve(
    shard_pipes: Vec<UnixPipe>,
    ced_listener: EndpointListener,
    gpu_listener: Option<EndpointListener>,
    criu_listener: EndpointListener,
) -> Result<()>
{
    let mut mem_store = image_store::mem::Store::default();
    let _ = drain_shards_into_img_store(&mut mem_store, shard_pipes)?;
    serve_img(&mut mem_store, ced_listener, gpu_listener, criu_listener)?;

    Ok(())
}

/// Description of the arguments can be found in main.rs
pub fn extract(images_dir: &Path,
    shard_pipes: Vec<UnixPipe>,
) -> Result<()>
{
    // extract on disk
    let mut file_store = image_store::fs::Store::new(images_dir);
    let _ = drain_shards_into_img_store(&mut file_store, shard_pipes)?;

    Ok(())
}
