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


all: criu-image-streamer

PREFIX ?= $(DESTDIR)/usr/local
BINDIR ?= $(PREFIX)/bin

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

target/$(BUILD)/criu-image-streamer: $(DEPS)
	$(CARGO) build $(BUILD_FLAGS)

criu-image-streamer: target/$(BUILD)/criu-image-streamer
	cp -a $< $@

install: target/$(BUILD)/criu-image-streamer
	install -m0755 $< $(BINDIR)/criu-image-streamer

uninstall:
	$(RM) $(addprefix $(BINDIR)/,criu-image-streamer)

test:
	$(CARGO) test $(BUILD_FLAGS) -- --test-threads=1 --nocapture

shellcheck:
	shellcheck -o all tests/integration.bats

shfmt:
	shfmt -w tests/integration.bats

integration-test: target/$(BUILD)/criu-image-streamer
	bats --jobs 10 tests/integration.bats

clean:
	rm -rf target criu-image-streamer

.PHONY: all clean install integration-test shellcheck shfmt test uninstall
