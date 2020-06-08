[![master](https://travis-ci.org/checkpoint-restore/criu-image-streamer.svg?branch=master)](https://travis-ci.org/checkpoint-restore/criu-image-streamer)

CRIU Image Streamer
====================

_criu-image-streamer_ enables streaming of images to and from
[CRIU](https://www.criu.org) during checkpoint/restore with low overhead.

It enables use of remote storage (e.g., S3, GCS) without buffering in local
storage, speeding up operations considerably. Fast checkpointing makes Google's
preemptible VM and Amazon Spot VM offerings more attractive: with streaming,
CRIU can checkpoint and evacuate even large processes within the tight eviction
deadlines (~30secs).


criu-image-streamer includes the following high-level features:

* **Extensible**: UNIX pipes are used for image transfers allowing integration
in various workloads and environments. One can build fast data pipelines to
performing compression, encryption, and remote storage access.

* **Image sharding**: When capturing a CRIU image, the image stream can be split
into multiple output shards. This helps maximizing the network throughput for
remote uploads and CPU utilization for compression/encryption.

* **Shard load balancing**: When capturing a CRIU image, the throughput of each
output shard is independently optimized. If a shard exhibits poor performance
(e.g., by hitting a slow disk), traffic is directed to other shards. This is
useful for reducing checkpoint tail latency when using many shards.

* **External file embedding**: Files that are not CRIU specific can be included
in the image. This can be used, for example, to incorporate a file system
tarball along with the CRIU image.

* **Low checkpoint overhead**: To maximize speed, we modified CRIU to send pipes
over its UNIX socket connection to transfer data. This allows the use of the
`splice()` system call for moving data pipe-to-pipe giving us a zero-copy
implementation. We measured **0.1 CPUsec/GB** of CPU usage and **3 MB** of
resident memory when capturing a 10 GB application on standard server hardware
of 2020.

* **Moderate restore overhead**: We measured **1.4 CPUsec/GB** of CPU usage
and **3 MB** of resident memory. In the future, we could switch to a zero-copy
implementation to greatly improve performance.

* **Reliable**: criu-image-streamer is written in Rust, avoiding common classes
of bugs often encountered when using other low-level languages.

Usage
-----

The CLI interface of criu-image-streamer is the following:

```
criu-image-streamer [OPTIONS] --images-dir <images-dir> <SUBCOMMAND>

OPTIONS:
    -D, --images-dir <images-dir>           Images directory where the CRIU UNIX socket is created during
                                            streaming operations.
    -s, --shard-fds <shard-fds>...          File descriptors of shards. Multiple fds may be passed as a comma
                                            separated list. Defaults to 0 or 1 depending on the operation.
    -e, --ext-file-fds <ext-file-fds>...    External files to incorporate/extract in/from the image. Format is
                                            filename:fd where filename corresponds to the name of the file, fd
                                            corresponds to the pipe sending or receiving the file content.
                                            Multiple external files may be passed as a comma separated list.
    -p, --progress-fd <progress-fd>         File descriptor where to report progress. Defaults to 2.


SUBCOMMANDS:
    capture    Capture a CRIU image
    extract    Extract a captured CRIU image

extract OPTIONS:
    --serve                                 Buffer the image in memory and serve to CRIU
```

`--images-dir` is used during operations to create a UNIX socket where CRIU can
connect to. That socket is used to exchange pipes for data transfers. The
directory is not used for storing data when streaming images to and from CRIU.

There are two modes of operation: capture and extract. Capture is used for
checkpointing and extract for restoring images. We show how these operations
are used with examples.

Example 1: On-the-fly compression to local storage
--------------------------------------------------

In this example, we show how to checkpoint/restore an application and
compress/decompress its image on-the-fly with the lz4 compressor.

### Checkpoint

```bash
sleep 10 & # The app to be checkpointed
APP_PID=$!

criu-image-streamer --images-dir /tmp capture | lz4 -f - /tmp/img.lz4 &
criu dump --images-dir /tmp --stream --shell-job --tree $APP_PID
```

### Restore

```bash
lz4 -d /tmp/img.lz4 - | criu-image-streamer --images-dir /tmp extract --serve &
criu restore --images-dir /tmp --stream --shell-job
```

Example 2: Extracting an image to local storage
-----------------------------------------------

Extracting a previously captured image to disk can be useful for inspection.
Using the `extract` command without `--serve` extract the image to disk instead
of waiting for CRIU to consume it from memory.

```bash
lz4 -d /tmp/img.lz4 - | criu-image-streamer --images-dir output_dir extract
```

Example 3: Multi-shard upload to the S3 remote storage
------------------------------------------------------

When compressing and uploading to S3, parallelism is beneficial both to
leverage multiple CPUs for compression, and multiple streams for maximizing
network throughput. Parallelism can be achieved by splitting the image stream
into multiple shards using the `--shard-fds` option.

### Checkpoint

```bash
sleep 10 & # The app to be checkpointed
APP_PID=$!

# The 'exec N>' syntax opens a new file descriptor in bash (not sh, not zsh).
exec 10> >(lz4 - - | aws s3 cp - s3://bucket/img-1.lz4)
exec 11> >(lz4 - - | aws s3 cp - s3://bucket/img-2.lz4)
exec 12> >(lz4 - - | aws s3 cp - s3://bucket/img-3.lz4)

criu-image-streamer --images-dir /tmp --shard-fds 10,11,12 capture &
criu dump --images-dir /tmp --stream --shell-job --tree $APP_PID
```

### Restore

```bash
exec 10< <(aws s3 cp s3://bucket/img-1.lz4 - | lz4 -d - -)
exec 11< <(aws s3 cp s3://bucket/img-2.lz4 - | lz4 -d - -)
exec 12< <(aws s3 cp s3://bucket/img-3.lz4 - | lz4 -d - -)

criu-image-streamer --shard-fds 10,11,12 --images-dir /tmp extract --serve &
criu restore --images-dir /tmp --stream --shell-job
```

Example 4: Incorporating a tarball into the image
-------------------------------------------------

Often, we wish to capture the file system along side the CRIU process image.
criu-image-streamer can weave in external files via the `--ext-file-fds` option.
In this example, We use `tar` to archive `/scratch` and include the tarball into
our final image.

### Checkpoint

```bash
mkdir -p /scratch/app
echo "app data to preserve" > /scratch/app/data

sleep 10 & # The app to be checkpointed
APP_PID=$!

# The 'exec N>' syntax opens a new file descriptor in bash (not sh, not zsh).
exec 20< <(tar -C / -vcpSf - /scratch/app)

criu-image-streamer --images-dir /tmp --ext-file-fds fs.tar:20 capture | lz4 -f - /tmp/img.lz4 &
criu dump --images-dir /tmp --stream --shell-job --tree $APP_PID
```

### Restore

```bash
rm -f /scratch/app/data

exec 20> >(tar -C / -vxf - --no-overwrite-dir)

lz4 -d /tmp/img.lz4 - | criu-image-streamer --images-dir /tmp --ext-file-fds fs.tar:20 extract --serve &
criu restore --images-dir /tmp --stream --shell-job

cat /scratch/app/data
```

**Important correctness consideration**: We are missing synchronization details
in this simplified example. For correctness, we should do the following:

* On checkpoint, we should start tarring the file system AFTER the application
has stopped. Otherwise, we risk a data race leading to data loss.

* On restore, we should only start CRIU after tar has finished restoring the
file system. Otherwise, we risk having CRIU try to access files that are not
yet present.

Synchronization
---------------

criu-image-streamer emits the following into the progress pipe, helpful for
synchronizing operations:

* During capture it emits the following messages:
  * `socket-init\n` to report that the UNIX socket is ready for CRIU to connect.
    At this point, CRIU is safe to be launched for dump.
  * `checkpoint-start\n` to report that the checkpoint has started.
    The application is now guaranteed to be in a stopped state. Starting tarring
    the file system is appropriate.
  * JSON formatted statistics defined below.

* During restore:
  * JSON formatted statistics defined below.
  * `socket-init\n` to report that the UNIX socket is ready for CRIU to connect.
    At this point, CRIU is safe to be launched for restore.

Transfer speed statistics
-------------------------

The progress pipe emits statistics related to shards with the following JSON
format. These statistics are helpful to compute transfer speeds.
The JSON blob is emitted as a single `\n` terminated line.

```javascript
{
  "shards": [
    {
      "size": u64, // Total size of shard in bytes
      "transfer_duration_millis": u128, // Total time to transfer data
    },
    ...
  ]
}
```

Installation
------------

### Build

The Rust toolchain must be installed as a prerequisite.
Run `make`, or use `cargo build --release` to build the project.

### Deploy

Copy the built binary to the destination host. It requires no library except
libc. Change kernel settings to the following for optimal performance.

```bash
echo 0 > /proc/sys/fs/pipe-user-pages-soft
echo 0 > /proc/sys/fs/pipe-user-pages-hard
echo $((4*1024*1024)) > /proc/sys/fs/pipe-max-size
```

Note that during checkpointing, pages in the pipe buffers are not consuming
memory. Because CRIU uses `vmsplice()` and criu-image-streamer uses `splice()`,
the content in the pipes are pointing directly to the application memory.

Tests
-----

We provide a test suite located in `tests/`. You may run it with `cargo test --
--test-threads=1`, or `make test`.

Limitations
-----------

* Incremental checkpoints are not supported.
* Most CLI options must be passed _before_ the capture/extract subcommand.
* Shards must be UNIX pipes. For regular files support, `cat` or `pv` (faster)
may be used as a pipe adapter.
* Using an older Linux kernel can lead to memory corruption.
We tested version 4.14.67 from the stable tree, and have seen memory corruption.
We tested version 4.14.121 and seen no issues. 4.15.0-1037 is problematic.
It appears that this [kernel bug fix](https://github.com/torvalds/linux/commit/1bdc347)
is the remedy. Run `cargo test splice` to test if criu-image-streamer is
affected by the bug on your platform.

Acknowledgments
---------------
* Author: Nicolas Viennot [@nviennot](https://github.com/nviennot)
* Reviewer: Vitaly Davidovich [@vitalyd](https://github.com/vitalyd)
* Reviewer: Peter Burka [@pburka](https://github.com/pburka)
* Developed as a [Two Sigma Open Source](https://opensource.twosigma.com) initiative

License
-------

criu-image-streamer is licensed under the
[Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).
