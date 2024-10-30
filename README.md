Cedana Image Streamer
====================

_cedana-image-streamer_ enables streaming of images to and from
[Cedana](https://github.com/cedana/cedana) during checkpoint/restore with low overhead.

This is a maintained fork of https://github.com/checkpoint-restore/criu-image-streamer. 

Usage
-----

**Note**: _cedana-image-streamer_ requires [this fork](https://github.com/lianakoleva/criu) of CRIU.

To build the _cedana-image-streamer_ executable:
```
cargo build --release --bin cedana-image-streamer
```
Place it in your PATH:
```
sudo cp target/release/cedana-image-streamer /usr/bin
```
To checkpoint with streaming:
```
cedana dump job workload -d /dumpdir --stream
```
To restore with streaming:
```
cedana restore job workload --stream
```

License
-------
cedana-image-streamer is licensed under the [Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

criu-image-streamer is originally licensed under the
[Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

