Cedana Image Streamer
====================

_cedana-image-streamer_ enables streaming of images to and from
[Cedana](https://github.com/cedana/cedana) during checkpoint/restore with low overhead.

This is a maintained fork of https://github.com/checkpoint-restore/criu-image-streamer. 

Usage
-----

**Note**: _cedana-image-streamer_ requires [this fork](https://github.com/cedana/criu) of CRIU.

To build the _cedana-image-streamer_ executable:
```sh
make
```

For installation see [locally built plugins](https://docs.cedana.ai/daemon/get-started/plugins#locally-built-plugins). For usage, check out the [checkpoint/restore streaming](https://docs.cedana.ai/daemon/guides/cr-4).

License
-------
cedana-image-streamer is licensed under the [Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

criu-image-streamer is originally licensed under the
[Apache 2.0 license](https://www.apache.org/licenses/LICENSE-2.0).

