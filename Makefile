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

COVERAGE_PATH ?= $(shell pwd)/.coverage

# Coverage tool - uses cargo-llvm-cov for source-based coverage
LLVM_COV = $(CARGO) llvm-cov

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
	shellcheck -o all tests/integration.bats tests/prepare-fedora-test-environment.sh proto/sync_criu_proto_files.sh

shfmt:
	shfmt -w tests/integration.bats tests/prepare-fedora-test-environment.sh proto/sync_criu_proto_files.sh

integration-test: target/$(BUILD)/criu-image-streamer
	bats --jobs 10 tests/integration.bats

test-junit: target/$(BUILD)/criu-image-streamer
	$(CARGO) nextest run $(BUILD_FLAGS) --test-threads=1
	bats -F junit --jobs 10 tests/integration.bats > tests/junit-integration.xml

clean:
	rm -rf target criu-image-streamer
	rm -rf $(COVERAGE_PATH)
	rm -f tests/junit.xml tests/junit-integration.xml

# Coverage targets - requires cargo-llvm-cov: cargo install cargo-llvm-cov
#
# Coverage workflow:
# 1. Unit tests: cargo-llvm-cov runs tests and collects coverage
# 2. Integration tests: Run bats with instrumented binary, collect profraw files
# 3. Merge: Combine all coverage data and generate reports
#
# Usage:
#   make coverage           - Run all tests with coverage
#   make coverage-unit      - Run only unit tests with coverage
#   make coverage-integration - Run only integration tests with coverage
#   make coverage-html      - Generate HTML coverage report

# Run unit tests with coverage
coverage-unit:
	mkdir -p $(COVERAGE_PATH)
	$(LLVM_COV) test --release --no-fail-fast -- --test-threads=1 --nocapture
	@echo "Unit test coverage complete"

# Run integration tests with coverage-instrumented binary
# Sets coverage environment variables directly for proper instrumentation
coverage-integration:
	mkdir -p $(COVERAGE_PATH)
	CARGO_INCREMENTAL=0 \
	RUSTFLAGS="-C instrument-coverage" \
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
		$(CARGO) build --release
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
	COVERAGE=1 COVERAGE_PATH=$(COVERAGE_PATH) \
		bats --jobs 10 tests/integration.bats
	@echo "Integration test coverage complete"

# Run all tests with coverage and generate combined report
coverage:
	mkdir -p $(COVERAGE_PATH)
	# Clean any previous coverage data
	$(LLVM_COV) clean --workspace
	# Run unit tests with coverage (profraw files go to cargo-llvm-cov's target dir)
	$(LLVM_COV) test --release --no-fail-fast --no-report -- --test-threads=1 --nocapture
	# Build instrumented binary for integration tests
	CARGO_INCREMENTAL=0 \
	RUSTFLAGS="-C instrument-coverage" \
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
		$(CARGO) build --release
	# Run integration tests with instrumented binary
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
	COVERAGE=1 COVERAGE_PATH=$(COVERAGE_PATH) \
		bats --jobs 10 tests/integration.bats
	# Merge integration test profraw files into cargo-llvm-cov's target dir
	llvm-profdata merge -sparse $(COVERAGE_PATH)/integration-*.profraw \
		-o target/llvm-cov-target/criu_image_streamer-integration.profdata || true
	# Generate combined report from all collected coverage data
	@echo ""
	@echo "=== Coverage Summary ==="
	$(LLVM_COV) report --release
	$(LLVM_COV) --release --no-run --lcov --output-path $(COVERAGE_PATH)/coverage.lcov
	@echo ""
	@echo "Coverage data written to $(COVERAGE_PATH)/"
	@echo "  - LCOV format: $(COVERAGE_PATH)/coverage.lcov"

# Generate HTML coverage report
coverage-html:
	mkdir -p $(COVERAGE_PATH)
	# Clean any previous coverage data
	$(LLVM_COV) clean --workspace
	# Run unit tests with coverage
	$(LLVM_COV) test --release --no-fail-fast --no-report -- --test-threads=1 --nocapture
	# Build instrumented binary for integration tests
	CARGO_INCREMENTAL=0 \
	RUSTFLAGS="-C instrument-coverage" \
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
		$(CARGO) build --release
	# Run integration tests with instrumented binary
	LLVM_PROFILE_FILE="$(COVERAGE_PATH)/integration-%p-%m.profraw" \
	COVERAGE=1 COVERAGE_PATH=$(COVERAGE_PATH) \
		bats --jobs 10 tests/integration.bats
	# Merge integration test profraw files
	llvm-profdata merge -sparse $(COVERAGE_PATH)/integration-*.profraw \
		-o target/llvm-cov-target/criu_image_streamer-integration.profdata || true
	# Generate HTML report
	$(LLVM_COV) report --release --html --output-dir $(COVERAGE_PATH)/html
	@echo ""
	@echo "HTML report: $(COVERAGE_PATH)/html/index.html"

.PHONY: all clean install integration-test shellcheck shfmt test test-junit uninstall
.PHONY: coverage coverage-unit coverage-integration coverage-html
