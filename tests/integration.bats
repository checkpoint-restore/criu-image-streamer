#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
#
# Integration tests for criu-image-streamer based on README examples.
# These tests require: criu, lz4, and root privileges.
#
# Run with: sudo bats tests/integration.bats
# Run with coverage: make coverage-integration (uses cargo-llvm-cov environment)

# shellcheck disable=SC2312

# Path to the criu-image-streamer binary
# When running with coverage (LLVM_PROFILE_FILE set by cargo-llvm-cov),
# the binary will automatically write coverage data
STREAMER="${STREAMER:-./target/release/criu-image-streamer}"

setup() {
	# Create a unique temporary directory for each test
	TEST_DIR=$(mktemp -d -t criu-streamer-test.XXXXXX)
	IMAGES_DIR="${TEST_DIR}/images"
	# Use unique sleep time per test to allow parallel execution
	# shellcheck disable=SC2154
	SLEEP_TIME=$((4242 + BATS_TEST_NUMBER))
	mkdir -p "${IMAGES_DIR}"

	# Build if binary doesn't exist
	if [[ ! -x "${STREAMER}" ]]; then
		cargo build --release
	fi
}

teardown() {
	# Kill any lingering sleep processes from tests
	pkill -f "sleep ${SLEEP_TIME}" 2>/dev/null || true

	# Clean up temporary directory
	if [[ -n "${TEST_DIR}" && -d "${TEST_DIR}" ]]; then
		rm -rf "${TEST_DIR}"
	fi
}

# Helper to check if we're running as root
check_root() {
	if [[ ${EUID} -ne 0 ]]; then
		skip "This test requires root privileges"
	fi
}

# Helper to check if criu is available
check_criu() {
	if ! command -v criu &>/dev/null; then
		skip "criu is not installed"
	fi
}

# Helper to check if lz4 is available
check_lz4() {
	if ! command -v lz4 &>/dev/null; then
		skip "lz4 is not installed"
	fi
}

# README Example 1: On-the-fly compression to local storage
# Checkpoint a process and compress with lz4
@test "Example 1: Checkpoint with lz4 compression" {
	check_root
	check_criu
	check_lz4

	# Start a simple background process to checkpoint
	sleep "${SLEEP_TIME}" &
	APP_PID=$!

	# Give the process time to start
	sleep 0.5

	# Verify process is running
	kill -0 "${APP_PID}" 2>/dev/null || fail "Test process failed to start"

	# Checkpoint with lz4 compression
	"${STREAMER}" --images-dir "${IMAGES_DIR}" capture | lz4 -f - "${TEST_DIR}/img.lz4" &
	STREAMER_PID=$!

	# Wait for socket to be ready (streamer outputs "socket-init" to stderr by default)
	sleep 1

	# Run CRIU dump
	run criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"

	# Wait for streamer to finish
	wait "${STREAMER_PID}"

	# Verify the compressed image was created
	[[ -f "${TEST_DIR}/img.lz4" ]]
	[[ -s "${TEST_DIR}/img.lz4" ]]

	echo "Compressed image size: $(stat -c%s "${TEST_DIR}/img.lz4") bytes"
}

# README Example 1: Restore from compressed image
@test "Example 1: Restore with lz4 decompression" {
	check_root
	check_criu
	check_lz4

	# First, create a checkpoint to restore from
	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	"${STREAMER}" --images-dir "${IMAGES_DIR}" capture | lz4 -f - "${TEST_DIR}/img.lz4" &
	STREAMER_PID=$!
	sleep 1

	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"
	wait "${STREAMER_PID}"

	# Clean up the images dir for restore
	rm -rf "${IMAGES_DIR}"
	mkdir -p "${IMAGES_DIR}"

	# Now restore from the compressed image
	lz4 -d "${TEST_DIR}/img.lz4" - | "${STREAMER}" --images-dir "${IMAGES_DIR}" serve &
	STREAMER_PID=$!

	# Wait for socket to be ready
	sleep 1

	# Run CRIU restore with --restore-detached so it returns immediately
	run criu restore --images-dir "${IMAGES_DIR}" --stream --shell-job --restore-detached -v4 --log-file /tmp/1.log
	restore_status=${status}

	# Wait for streamer to finish
	wait "${STREAMER_PID}" || true

	# Verify the process was restored with the same PID
	kill -0 "${APP_PID}" 2>/dev/null || fail "Restored process not running with expected PID ${APP_PID}"

	# Clean up the restored process
	kill "${APP_PID}" 2>/dev/null || true

	[[ ${restore_status} -eq 0 ]]
}

# README Example 2: Extracting an image to local storage
@test "Example 2: Extract image to disk" {
	check_root
	check_criu
	check_lz4

	# Create a checkpoint first
	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	"${STREAMER}" --images-dir "${IMAGES_DIR}" capture | lz4 -f - "${TEST_DIR}/img.lz4" &
	STREAMER_PID=$!
	sleep 1

	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"
	wait "${STREAMER_PID}"

	# Create output directory for extraction
	OUTPUT_DIR="${TEST_DIR}/extracted"
	mkdir -p "${OUTPUT_DIR}"

	# Extract the image to disk
	lz4 -d "${TEST_DIR}/img.lz4" - | "${STREAMER}" --images-dir "${OUTPUT_DIR}" extract

	# Verify that image files were extracted
	[[ -d "${OUTPUT_DIR}" ]]

	# Count extracted files
	file_count=$(find "${OUTPUT_DIR}" -type f | wc -l)
	echo "Extracted ${file_count} files"
	[[ ${file_count} -gt 0 ]]

	# Check for common CRIU image files (use ls to check glob pattern)
	[[ -f "${OUTPUT_DIR}/inventory.img" ]] || [[ -f "${OUTPUT_DIR}/pstree.img" ]] || ls "${OUTPUT_DIR}"/core-*.img &>/dev/null
}

# README Example 3: Multi-shard checkpoint (simplified, using local files instead of S3)
@test "Example 3: Multi-shard checkpoint" {
	check_root
	check_criu
	check_lz4

	# Start a process to checkpoint
	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	SHARD1="${TEST_DIR}/shard1.lz4"
	SHARD2="${TEST_DIR}/shard2.lz4"
	SHARD3="${TEST_DIR}/shard3.lz4"

	# Run streamer with file descriptors set up in a bash subshell
	# This ensures fd inheritance works properly
	bash -c '
        exec 10> >(lz4 - "'"${SHARD1}"'")
        exec 11> >(lz4 - "'"${SHARD2}"'")
        exec 12> >(lz4 - "'"${SHARD3}"'")
        "'"${STREAMER}"'" --images-dir "'"${IMAGES_DIR}"'" --shard-fds 10,11,12 capture
    ' &
	STREAMER_PID=$!

	sleep 1

	# Run CRIU dump
	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"

	# Wait for streamer to finish
	wait "${STREAMER_PID}" || true

	# Give lz4 processes time to finish writing
	sleep 1

	# Verify shards were created
	echo "Shard 1 size: $(stat -c%s "${SHARD1}" 2>/dev/null || echo 0) bytes"
	echo "Shard 2 size: $(stat -c%s "${SHARD2}" 2>/dev/null || echo 0) bytes"
	echo "Shard 3 size: $(stat -c%s "${SHARD3}" 2>/dev/null || echo 0) bytes"

	# At least one shard should have data
	total_size=0
	for shard in "${SHARD1}" "${SHARD2}" "${SHARD3}"; do
		if [[ -f "${shard}" ]]; then
			size=$(stat -c%s "${shard}")
			total_size=$((total_size + size))
		fi
	done
	[[ ${total_size} -gt 0 ]]
}

# README Example 3: Multi-shard restore
@test "Example 3: Multi-shard restore" {
	check_root
	check_criu
	check_lz4

	# First create a multi-shard checkpoint
	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	SHARD1="${TEST_DIR}/shard1.lz4"
	SHARD2="${TEST_DIR}/shard2.lz4"
	SHARD3="${TEST_DIR}/shard3.lz4"

	# Run streamer with file descriptors set up in a bash subshell
	bash -c '
        exec 10> >(lz4 - "'"${SHARD1}"'")
        exec 11> >(lz4 - "'"${SHARD2}"'")
        exec 12> >(lz4 - "'"${SHARD3}"'")
        "'"${STREAMER}"'" --images-dir "'"${IMAGES_DIR}"'" --shard-fds 10,11,12 capture
    ' &
	STREAMER_PID=$!
	sleep 1

	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"

	wait "${STREAMER_PID}" || true
	sleep 1

	# Clean up images dir for restore
	rm -rf "${IMAGES_DIR}"
	mkdir -p "${IMAGES_DIR}"

	# Run streamer for restore with file descriptors set up in a bash subshell
	bash -c '
        exec 10< <(lz4 -d "'"${SHARD1}"'" -)
        exec 11< <(lz4 -d "'"${SHARD2}"'" -)
        exec 12< <(lz4 -d "'"${SHARD3}"'" -)
        "'"${STREAMER}"'" --shard-fds 10,11,12 --images-dir "'"${IMAGES_DIR}"'" serve
    ' &
	STREAMER_PID=$!
	sleep 1

	# Run CRIU restore with --restore-detached so it returns immediately
	run criu restore --images-dir "${IMAGES_DIR}" --stream --shell-job --restore-detached
	restore_status=${status}

	# Wait for streamer to finish
	wait "${STREAMER_PID}" || true

	# Verify the process was restored with the same PID
	kill -0 "${APP_PID}" 2>/dev/null || fail "Restored process not running with expected PID ${APP_PID}"

	# Clean up the restored process
	kill "${APP_PID}" 2>/dev/null || true

	[[ ${restore_status} -eq 0 ]]
}

# README Example 4: Incorporating a tarball into the image
@test "Example 4: Checkpoint with external tarball" {
	check_root
	check_criu
	check_lz4

	# Create test data to tar
	SCRATCH_DIR="${TEST_DIR}/scratch/app"
	mkdir -p "${SCRATCH_DIR}"
	echo "app data to preserve" >"${SCRATCH_DIR}/data"
	echo "more app data" >"${SCRATCH_DIR}/config"

	# Start a process to checkpoint
	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	# Create a FIFO for the tar data
	mkfifo "${TEST_DIR}/tar_pipe"

	# Start tar to send data through the pipe
	tar -C "${TEST_DIR}" -cpf - scratch/app >"${TEST_DIR}/tar_pipe" &
	TAR_PID=$!

	# Open file descriptor for the tar pipe
	exec 20<"${TEST_DIR}/tar_pipe"

	# Run streamer with external file
	"${STREAMER}" --images-dir "${IMAGES_DIR}" --ext-file-fds fs.tar:20 capture | lz4 -f - "${TEST_DIR}/img.lz4" &
	STREAMER_PID=$!

	sleep 1

	# Run CRIU dump
	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"

	exec 20<&-
	wait "${TAR_PID}" || true
	wait "${STREAMER_PID}" || true

	# Verify the image was created and contains the external file
	[[ -f "${TEST_DIR}/img.lz4" ]]
	[[ -s "${TEST_DIR}/img.lz4" ]]

	echo "Image with tarball size: $(stat -c%s "${TEST_DIR}/img.lz4") bytes"
}

# README Example 4: Restore with external tarball extraction
@test "Example 4: Restore with external tarball extraction" {
	check_root
	check_criu
	check_lz4

	# First create checkpoint with tarball
	SCRATCH_DIR="${TEST_DIR}/scratch/app"
	mkdir -p "${SCRATCH_DIR}"
	echo "app data to preserve" >"${SCRATCH_DIR}/data"

	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	mkfifo "${TEST_DIR}/tar_pipe"
	tar -C "${TEST_DIR}" -cpf - scratch/app >"${TEST_DIR}/tar_pipe" &
	exec 20<"${TEST_DIR}/tar_pipe"

	"${STREAMER}" --images-dir "${IMAGES_DIR}" --ext-file-fds fs.tar:20 capture | lz4 -f - "${TEST_DIR}/img.lz4" &
	STREAMER_PID=$!
	sleep 1

	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"
	exec 20<&-
	wait "${STREAMER_PID}" || true
	wait

	# Remove the original data
	rm -rf "${TEST_DIR}/scratch"

	# Clean up images dir for restore
	rm -rf "${IMAGES_DIR}"
	mkdir -p "${IMAGES_DIR}"

	# Create output directory for extracted tar
	RESTORE_DIR="${TEST_DIR}/restored"
	mkdir -p "${RESTORE_DIR}"

	# Create FIFO for tar extraction
	rm -f "${TEST_DIR}/tar_pipe"
	mkfifo "${TEST_DIR}/tar_pipe"

	# Start tar to extract data from the pipe
	tar -C "${RESTORE_DIR}" -xf - <"${TEST_DIR}/tar_pipe" &
	TAR_PID=$!

	# Open file descriptor for writing
	exec 20>"${TEST_DIR}/tar_pipe"

	# Restore with external file extraction
	lz4 -d "${TEST_DIR}/img.lz4" - | "${STREAMER}" --images-dir "${IMAGES_DIR}" --ext-file-fds fs.tar:20 serve &
	STREAMER_PID=$!
	sleep 1

	# Run CRIU restore with --restore-detached so it returns immediately
	run criu restore --images-dir "${IMAGES_DIR}" --stream --shell-job --restore-detached
	restore_status=${status}

	exec 20>&-
	wait "${TAR_PID}" || true
	wait "${STREAMER_PID}" || true
	wait || true

	# Verify the process was restored with the same PID
	kill -0 "${APP_PID}" 2>/dev/null || fail "Restored process not running with expected PID ${APP_PID}"

	# Clean up the restored process
	kill "${APP_PID}" 2>/dev/null || true

	# Verify the tarball was extracted
	[[ -f "${RESTORE_DIR}/scratch/app/data" ]]
	content=$(cat "${RESTORE_DIR}/scratch/app/data")
	[[ "${content}" == "app data to preserve" ]]

	[[ ${restore_status} -eq 0 ]]
}

# Test progress output
@test "Progress output: socket-init message" {
	check_root
	check_criu

	sleep "${SLEEP_TIME}" &
	APP_PID=$!
	sleep 0.5

	# Capture progress output to a file (progress goes to stderr by default)
	PROGRESS_FILE="${TEST_DIR}/progress.txt"

	# Run streamer piped through cat (shards must be pipes, not regular files)
	# Capture stderr (progress) to file
	"${STREAMER}" --images-dir "${IMAGES_DIR}" capture 2>"${PROGRESS_FILE}" | cat >"${TEST_DIR}/img.bin" &
	STREAMER_PID=$!

	sleep 1

	criu dump --images-dir "${IMAGES_DIR}" --stream --shell-job --network-lock skip --tree "${APP_PID}"

	wait "${STREAMER_PID}" || true

	# Check for socket-init message
	grep -q "socket-init" "${PROGRESS_FILE}"

	# Check for checkpoint-start message
	grep -q "checkpoint-start" "${PROGRESS_FILE}"

	# Check for JSON stats (should have "shards" key)
	grep -q "shards" "${PROGRESS_FILE}"
}

# Basic smoke test without CRIU
@test "Smoke test: streamer --help" {
	run "${STREAMER}" --help
	[[ ${status} -eq 0 ]]
	[[ "${output}" == *"capture"* ]]
	[[ "${output}" == *"serve"* ]]
	[[ "${output}" == *"extract"* ]]
}

# Test that streamer correctly reports version or help
@test "Smoke test: streamer subcommand help" {
	run "${STREAMER}" --images-dir /tmp capture --help
	[[ ${status} -eq 0 ]]
}
