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
    collections::{BinaryHeap},
    os::unix::io::AsRawFd,
    time::Instant,
    cmp::{min, max},
    sync::Once,
    rc::Rc,
};
use crate::{
    poller::{Poller, EpollFlags},
    endpoint_connection::{EndpointListener, EndpointConnection},
    unix_pipe::{UnixPipe, UnixPipeImpl},
    util::*,
    image,
    image::marker,
    impl_ord_by,
    prnt,
};
use anyhow::Result;

// When CRIU dumps an application, it first connects to our UNIX socket. CRIU will send us many
// image files during the dumping process. To send an image file, it sends a protobuf request that
// contains the filename. Immediately after this message, it sends a file descriptor of a pipe
// which we can use to receive the content of the corresponding file. We stream the
// content of these image files to an array of outputs, called shards. The shards are typically
// a compression and upload stage (e.g., `lz4 | aws s3 cp - s3://destination`). The number of
// shards is typically 4, and less than 32. Image file sizes can vary widely (1KB to +10GB) and are
// sometimes sent interleaved (e.g., pages-X.img and pagemap-X.img).
//
// We move data from CRIU into shards without copying data to user-land for performance. For this,
// we use the splice() system call. This makes the implementation tricky because we also want to
// load-balance data to the shards.
//
// If we were to use a round-robin scheduling algorithm, we would get terrible slowdown if one of
// the shards were to be slow. To avoid this problem, we load-balance with the available throughput
// of each shard. To do so, we must know the available buffer space in each shard. For this we use
// the ioctl fionread(). It allows to measure the amount of data in a pipe. Due to this
// unpredictable data scheduling, we mark each data chunk with a sequence number that is used to
// reorder the data stream during restore.
//
// The shard data streams are comprised of markers followed by an optional data payload. The format
// of the markers is described in ../proto/image.proto


/// CRIU has difficulties if the pipe size is bigger than 4MB.
/// Note that the following pipe buffers are not actually using memory. The content of the pipe is
/// just a list of pointers to the application memory page, which is already allocated as CRIU does
/// a vmsplice(..., SPLICE_F_GIFT) when providing data.
const CPU_PIPE_DESIRED_CAPACITY: i32 = 4*MB as i32;
const GPU_PIPE_DESIRED_CAPACITY: i32 = 16*MB as i32;

/// Large buffers size improves performance as it allows us to increase the size of our chunks.
/// 1MB provides excellent performance.
#[allow(clippy::identity_op)]
const CPU_SHARD_PIPE_DESIRED_CAPACITY: i32 = 2*MB as i32;
const GPU_SHARD_PIPE_DESIRED_CAPACITY: i32 = 16*MB as i32;

/// Storing more shards per pipe reduces stalling.
const SHARDS_PER_PIPE: i32 = 8;

/// An `ImageFile` represents a file coming from CRIU.
/// The complete CRIU image is comprised of many of these files.
struct ImageFile {
    /// Incoming pipe from CRIU
    pipe: UnixPipe,
    /// Associated filename (e.g., "pages-3.img")
    filename: Rc<str>,
}

impl ImageFile {
    /// typename: false = criu, true = gpu
    pub fn new(filename: String, mut pipe: UnixPipe, typename: bool) -> Self {
        // Try setting the pipe capacity. Failing is okay, it's just for better performance.
        let _ = pipe.set_capacity(if typename { GPU_PIPE_DESIRED_CAPACITY } else { CPU_PIPE_DESIRED_CAPACITY });
        let filename = Rc::from(filename);
        Self { pipe, filename }
    }
}

/// A `Shard` is a pipe whose endpoint goes to the upload process (e.g., aws s3)
/// We keep track of the in-kernel buffer capacity to optimize performance.
struct Shard {
    /// Outgoing pipe to the uploader
    pipe: UnixPipe,
    /// `remaining_space` is a lower bound of the in-kernel pipe remaining space. As the upload
    /// process consume the pipe, the true `remaining_space` increases, but we don't know about it
    /// until we call fionread(). Note that we keep this signed, as it can potentially go negative
    /// as we overestimate the size of our writes (see CHUNK_MARKER_MAX_SIZE).
    remaining_space: i32,
    /// Total bytes written to the pipe, useful for producing stats.
    bytes_written: u64,
}

impl Shard {
    pub fn new(pipe: UnixPipe) -> Result<Self> {
        Ok(Self { pipe, remaining_space: 0, bytes_written: 0 })
    }

    pub fn refresh_remaining_space(&mut self, pipe_capacity: i32) -> Result<()> {
        let pipe_len = self.pipe.fionread()?;
        self.remaining_space = pipe_capacity - pipe_len;
        Ok(())
    }

    /// May silently fail to set capacity, use with caution
    pub fn set_shard_capacity(&mut self, capacity: i32) {
        let _ = self.pipe.set_capacity(capacity);
    }
}

// This gives ordering to `Shard` over its `remaining_space` field, useful for the binary heap
// (max-heap) in `ImageSerializer`. The Shard with the largest `remaining_space` goes first. On
// ambiguities, we order by file descriptor providing a total order.
impl_ord_by!(Shard, |a: &Self, b: &Self| a.remaining_space.cmp(&b.remaining_space)
    .then(a.pipe.as_raw_fd().cmp(&b.pipe.as_raw_fd())));

/// The image serializer reads data from CRIU's image files pipes, chunks the data, and writes into
/// shard pipes. Each chunk is written to the shard that has the most room available in its pipe.
/// We keep track of which shard has the most room with a binary heap.
/// Chunks are ordered by a sequence number. Semantically, the sequence number should be per image
/// file, but for simplicity, we use a global sequence number. It makes the implementation easier,
/// esp. on the deserializer side.
struct ImageSerializer<'a> {
    shards: BinaryHeap<&'a mut Shard>,
    shard_pipe_capacity: i32, // constant
    seq: u64,
    current_filename: Option<Rc<str>>,
}

struct Chunk<'a> {
    marker: image::Marker,
    data: Option<(&'a mut ImageFile, i32)>,
}

/// Chunks are preceded by a header that we call marker. Chunk markers take an entire page in
/// kernel space as it is followed by spliced data.
static CHUNK_MARKER_KERNEL_SIZE: &PAGE_SIZE = &PAGE_SIZE;

impl<'a> ImageSerializer<'a> {
    pub fn new(shards: &'a mut [Shard], shard_pipe_capacity: i32) -> Self {
        assert!(!shards.is_empty());
        Self {
            shard_pipe_capacity,
            shards: shards.iter_mut().collect(),
            current_filename: None,
            seq: 0,
        }
    }

    fn resize(&mut self, new_capacity: i32) -> Result<()> {
        self.shards = self.shards.drain()
            .map(|shard| {
                shard.set_shard_capacity(new_capacity);
                Ok(shard)
            })
            .collect::<Result<_>>()?;
        Ok(())
    }

    fn refresh_all_shard_remaining_space(&mut self) -> Result<()> {
        // We wish to mutate all the elements of the BinaryHeap.
        // We tear the existing one down and build a fresh one to reduce insertion cost.
        let shard_pipe_capacity = self.shard_pipe_capacity;
        self.shards = self.shards.drain()
            .map(|shard| {
                shard.refresh_remaining_space(shard_pipe_capacity)?;
                Ok(shard)
            })
            .collect::<Result<_>>()?;
        Ok(())
    }

    fn gen_marker(&mut self, body: marker::Body) -> image::Marker {
        let seq = self.seq;
        self.seq += 1;
        image::Marker { seq, body: Some(body) }
    }

    /// When transferring bytes from the CRIU pipe to one of the shards, we do so with large chunks
    /// to reduce serialization overhead, but not too large to minimize blocking when writing for
    /// better load-balancing.
    fn chunk_max_data_size(&self) -> i32 {
        // If the shard pipe capacity is small, it's sad, but we need to send at least a page
        max(self.shard_pipe_capacity/SHARDS_PER_PIPE - **CHUNK_MARKER_KERNEL_SIZE as i32,
            *PAGE_SIZE as i32)
    }

    fn write_chunk(&mut self, chunk: Chunk) -> Result<()> {
        let data_size = match chunk.data {
            None => 0,
            Some((_, size)) => size,
        };

        // Estimate the space required in the shard pipe to write the marker and its data.
        let space_required = **CHUNK_MARKER_KERNEL_SIZE as i32 + data_size;

        // Check if the shard with the most remaining space is likely to block.
        // If so, refresh other pipes' remaining space to check for a better candidate.
        // Note: it's safe to unwrap(), because we always have one shard to work with.
        if self.shards.peek().unwrap().remaining_space < space_required {
            // We refresh the `remaining_space` of all shards instead of just refreshing the
            // current shard, otherwise we risk starvation of other shards without knowing it.
            self.refresh_all_shard_remaining_space()?;
        }

        // Pick the shard with the greatest remaining space for our write. We might block when we
        // write, but that's inevitable, and that's how our output is throttled.
        let mut shard = self.shards.peek_mut().unwrap();

        // 1) Write the chunk marker
        let marker_size = pb_write(&mut shard.pipe, &chunk.marker)?;

        // 2) and its associated data, if specified
        if let Some((img_file, _)) = chunk.data {
            img_file.pipe.splice_all(&mut shard.pipe, data_size as usize)?;
        }

        shard.bytes_written += marker_size as u64 + data_size as u64;
        shard.remaining_space -= space_required;
        // As the shard reference drops, the binary heap gets reordered. nice.

        Ok(())
    }

    fn maybe_write_filename_marker(&mut self, img_file: &ImageFile) -> Result<()> {
        // We avoid repeating the filename on sequential data chunks of the same file for
        // performance. We write the filename only when needed.
        let filename = &img_file.filename;
        match &self.current_filename {
            Some(current_filename) if current_filename == filename => {},
            _ => {
                self.current_filename = Some(Rc::clone(filename));
                let marker = self.gen_marker(marker::Body::Filename(filename.to_string()));
                self.write_chunk(Chunk { marker, data: None })?;
            }
        }

        Ok(())
    }

    /// Returns false if EOF of img_file is reached, true otherwise.
    pub fn drain_img_file(&mut self, img_file: &mut ImageFile) -> Result<bool> {
        let mut readable_len = img_file.pipe.fionread()?;

        // This code is only invoked when the poller reports that the image file's pipe is readable
        // (or errored), which is why we can detect EOF when fionread() returns 0.
        let is_eof = readable_len == 0;

        self.maybe_write_filename_marker(img_file)?;

        while readable_len > 0 {
            let data_size = min(readable_len, self.chunk_max_data_size());
            let marker = self.gen_marker(marker::Body::FileData(data_size as u32));
            self.write_chunk(Chunk { marker, data: Some((img_file, data_size)) })?;
            readable_len -= data_size;
        }

        if is_eof {
            let marker = self.gen_marker(marker::Body::FileEof(true));
            self.write_chunk(Chunk { marker, data: None })?;
        }

        Ok(!is_eof)
    }

    pub fn write_image_eof(&mut self) -> Result<()> {
        let marker = self.gen_marker(image::marker::Body::ImageEof(true));
        self.write_chunk(Chunk { marker, data: None })
    }
}


/// The description of arguments can be found in main.rs
pub fn capture(
    mut shard_pipes: Vec<UnixPipe>,
    gpu_listener: Option<EndpointListener>,
    criu_listener: EndpointListener,
    ced_listener: EndpointListener,
) -> Result<()>
{
    // First, we need to listen on the unix socket and notify the progress pipe that
    // we are ready. We do this ASAP because our controller is blocking on us to start CRIU.

    // The kernel may limit the number of allocated pages for pipes, we must do it before setting
    // the pipe size of external file pipes as shard pipes are more performance sensitive.
    let initial_capacity = if gpu_listener.is_some() { GPU_SHARD_PIPE_DESIRED_CAPACITY } else { CPU_SHARD_PIPE_DESIRED_CAPACITY };
    let shard_pipe_capacity = UnixPipe::increase_capacity(&mut shard_pipes, initial_capacity)?;
    let mut shards: Vec<Shard> = shard_pipes.into_iter().map(Shard::new).collect::<Result<_>>()?;

    // Setup the poller to monitor the server socket and image files' pipes
    enum PollType {
        Endpoint(EndpointConnection),
        ImageFile(ImageFile),
    }
    let mut poller = Poller::new()?;
    const EPOLL_CAPACITY: usize = 8;

    // Used to compute transfer speed. But the real start is when we call
    // `notify_checkpoint_start_once()`
    let mut start_time = Instant::now();
    let notify_checkpoint_start_once = Once::new();

    // The image serializer reads data from the image files, and writes it in chunks into shards.
    let mut img_serializer = ImageSerializer::new(&mut shards, shard_pipe_capacity);

    // We are ready to get to work.
    match gpu_listener {
        Some(g) => {
            // Accept cedana-gpu-controller's connection.
            let gpu = g.into_accept()?;
            prnt!("connected to gpu");
            poller.add(gpu.as_raw_fd(), PollType::Endpoint(gpu), EpollFlags::EPOLLIN)?;

            // Process all inputs (ext files, CRIU's connection, and CRIU's files) until they reach EOF.
            // As CRIU requests to write files, we receive new unix pipes that are added to the poller.
            // We use an epoll_capacity of 8. This doesn't really matter as the number of concurrent
            // connection is typically at most 2.
            while let Some((poll_key, poll_obj)) = poller.poll(EPOLL_CAPACITY)? {
                match poll_obj {
                    PollType::Endpoint(gpu) => {
                        match gpu.read_next_file_request()? {
                            Some(filename) => {
                                notify_checkpoint_start_once.call_once(|| {
                                    start_time = Instant::now();
                                });

                                prnt!("gpu filename: {:?}", &filename);
                                let pipe = gpu.recv_pipe()?;
                                let img_file = ImageFile::new(filename, pipe, true);
                                poller.add(img_file.pipe.as_raw_fd(), PollType::ImageFile(img_file),
                                        EpollFlags::EPOLLIN)?;
                            }
                            None => {
                                // We are done receiving file requests. We can close the socket.
                                // However, other files may still be transferring data.
                                poller.remove(poll_key)?;
                            }
                        }
                    }
                    PollType::ImageFile(img_file) => {
                        if !img_serializer.drain_img_file(img_file)? {
                            // EOF of the image file is reached. Note that the image file pipe file
                            // descriptor is closed automatically as it is owned by the poller.
                            poller.remove(poll_key)?;
                        }
                    }
                }
            }
            prnt!("finished listening to gpu");
            let _ = img_serializer.resize(CPU_SHARD_PIPE_DESIRED_CAPACITY);
        },
        None => { prnt!("not using gpu"); },
    }

    let criu = criu_listener.into_accept()?;
    prnt!("connected to criu");

    poller.add(criu.as_raw_fd(), PollType::Endpoint(criu), EpollFlags::EPOLLIN)?;

    while let Some((poll_key, poll_obj)) = poller.poll(EPOLL_CAPACITY)? {
        match poll_obj {
            PollType::Endpoint(criu) => {
                match criu.read_next_file_request()? {
                    Some(filename) => {
                        notify_checkpoint_start_once.call_once(|| {
                            start_time = Instant::now();
                        });

                        prnt!("criu filename: {:?}", &filename);
                        let pipe = criu.recv_pipe()?;
                        let img_file = ImageFile::new(filename, pipe, false);
                        poller.add(img_file.pipe.as_raw_fd(), PollType::ImageFile(img_file),
                                   EpollFlags::EPOLLIN)?;
                    }
                    None => {
                        // We are done receiving file requests. We can close the socket.
                        // However, other files may still be transferring data.
                        poller.remove(poll_key)?;
                    }
                }
            }
            PollType::ImageFile(img_file) => {
                if !img_serializer.drain_img_file(img_file)? {
                    // EOF of the image file is reached. Note that the image file pipe file
                    // descriptor is closed automatically as it is owned by the poller.
                    poller.remove(poll_key)?;
                }
            }
        }
    }
    prnt!("finished listening to criu");

    let ced = ced_listener.into_accept()?;
    prnt!("connected to daemon");
    poller.add(ced.as_raw_fd(), PollType::Endpoint(ced), EpollFlags::EPOLLIN)?;

    while let Some((poll_key, poll_obj)) = poller.poll(EPOLL_CAPACITY)? {
        match poll_obj {
            PollType::Endpoint(ced) => {
                match ced.read_next_file_request()? {
                    Some(filename) => {
                        prnt!("daemon filename: {:?}", &filename);
                        let pipe = ced.recv_pipe()?;
                        let img_file = ImageFile::new(filename, pipe, false);
                        poller.add(img_file.pipe.as_raw_fd(), PollType::ImageFile(img_file),
                                   EpollFlags::EPOLLIN)?;
                    }
                    None => {
                        // We are done receiving file requests. We can close the socket.
                        // However, other files may still be transferring data.
                        poller.remove(poll_key)?;
                    }
                }
            }
            PollType::ImageFile(img_file) => {
                if !img_serializer.drain_img_file(img_file)? {
                    // EOF of the image file is reached. Note that the image file pipe file
                    // descriptor is closed automatically as it is owned by the poller.
                    poller.remove(poll_key)?;
                }
            }
        }
    }
    prnt!("finished listening to daemon");
    img_serializer.write_image_eof()?;
    Ok(())
}
