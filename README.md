Cedana Image Streamer
====================

_cedana-image-streamer_ enables streaming of images to and from
[Cedana](https://github.com/cedana/cedana) during checkpoint/restore with low overhead.

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
