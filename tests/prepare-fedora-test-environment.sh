#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
#
# Prepare a Fedora container for running tests.
#
# This script assumes it is running in a privileged Fedora container:
#   podman run --privileged -it registry.fedoraproject.org/fedora:latest
#
# Usage: ./tests/prepare-fedora-test-environment.sh

set -euo pipefail

echo "=== Installing system dependencies ==="
dnf -y install make gcc protobuf-devel criu lz4 bats llvm ShellCheck shfmt

echo ""
echo "=== Installing Rust via rustup ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Source cargo environment for this script
# shellcheck source=/dev/null
. "${HOME}/.cargo/env"

echo ""
echo "=== Installing llvm-tools-preview component ==="
rustup component add llvm-tools-preview

echo ""
echo "=== Installing cargo-llvm-cov ==="
cargo install --locked cargo-llvm-cov

echo ""
echo "=== Installing cargo-nextest ==="
cargo install --locked cargo-nextest

echo ""
echo "=== Setup complete ==="
echo ""
echo "Before running tests, source the cargo environment:"
echo ""
echo "    . \"\${HOME}/.cargo/env\""
echo ""
echo "Then run tests with:"
echo ""
echo "    make test"
echo "    make test-junit"
echo "    make coverage"
echo ""
