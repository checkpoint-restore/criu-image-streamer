#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
#
# Prepare a Fedora container for running coverage tests.
#
# This script assumes it is running in a privileged Fedora container:
#   podman run --privileged -it registry.fedoraproject.org/fedora:latest
#
# Usage: ./tests/prepare-fedora-coverage-environment.sh

set -euo pipefail

echo "=== Installing system dependencies ==="
dnf -y install make gcc protobuf-devel criu lz4 bats llvm

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
cargo install cargo-llvm-cov

echo ""
echo "=== Setup complete ==="
echo ""
echo "Before running coverage, source the cargo environment:"
echo ""
echo "    . \"\${HOME}/.cargo/env\""
echo ""
echo "Then run coverage with:"
echo ""
echo "    make coverage"
echo ""
