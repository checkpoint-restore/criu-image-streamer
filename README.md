Cedana Image Streamer
====================

_cedana-image-streamer_ enables streaming of images to and from
[Cedana](https://github.com/cedana/cedana) during checkpoint/restore with low overhead.

This is a maintained fork of https://github.com/checkpoint-restore/criu-image-streamer. 

Usage
-----

**Note**: _cedana-image-streamer_ requires [this fork](https://github.com/cedana/criu) of CRIU.

To build the _cedana-image-streamer_ executable:
```
cargo build --release --bin cedana-image-streamer
```
Place it in your PATH:
```
sudo cp target/release/cedana-image-streamer /usr/bin
```
To checkpoint with streaming (specify number of pipes `n`):
```
cedana dump job workload -d /dumpdir --stream n
```
To restore with streaming (same `n`):
```
cedana restore job workload --stream n
```

License
-------
cedana-image-streamer is licensed under the [Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

criu-image-streamer is originally licensed under the
[Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

