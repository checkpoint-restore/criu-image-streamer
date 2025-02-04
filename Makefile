#       Copyright 2020 Two Sigma Investments, LP.
#
#       Licensed under the Apache License, Version 2.0 (the "License");
#       you may not use this file except in compliance with the License.
#       You may obtain a copy of the License at
#
#           http://www.apache.org/licenses/LICENSE-2.0
#
#       Unless required by applicable law or agreed to in writing, software
#       distributed under the License is distributed on an "AS IS" BASIS,
#       WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#       See the License for the specific language governing permissions and
#       limitations under the License.


all: cedana-image-streamer

PREFIX ?= $(DESTDIR)/usr/local
BINDIR ?= $(PREFIX)/bin
VERSION=$(shell git describe --tags --always | sed 's/^v//')

BUILD ?= release

BUILD_FLAGS=

ifeq ($(BUILD),release)
	BUILD_FLAGS+=--release
endif

DEPS = $(wildcard src/*.rs src/**/*.rs) Cargo.toml

CARGO=$(HOME)/.cargo/bin/cargo
ifeq (,$(wildcard $(CARGO)))
	CARGO=cargo
endif

target/$(BUILD)/cedana-image-streamer: $(DEPS)
	@curr_version=$$(grep '^version' Cargo.toml | cut -d'"' -f2 | head -n1); \
	sed -i 's/^version = ".*"/version = "$(VERSION)"/' Cargo.toml; \
	$(CARGO) build $(BUILD_FLAGS) || (sed -i 's/^version = ".*"/version = "'$$curr_version'"/' Cargo.toml && exit 1); \
	sed -i 's/^version = ".*"/version = "'$$curr_version'"/' Cargo.toml

cedana-image-streamer: target/$(BUILD)/cedana-image-streamer
	cp -a $< $@

install: target/$(BUILD)/cedana-image-streamer
	install -m0755 $< $(BINDIR)/cedana-image-streamer

uninstall:
	$(RM) $(addprefix $(BINDIR)/,cedana-image-streamer)

test:
	$(CARGO) test $(BUILD_FLAGS) -- --test-threads=1 --nocapture

clean:
	rm -rf target cedana-image-streamer

.PHONY: all clean install test uninstall
