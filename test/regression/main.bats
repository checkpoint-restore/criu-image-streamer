#!/usr/bin/env bats

load helper.bash

setup_file() {
    BATS_NO_PARALLELIZE_WITHIN_FILE=true
}

setup() {
    # assuming WD is the root of the project
    start_cedana

    # get the containing directory of this file
    # use $BATS_TEST_FILENAME instead of ${BASH_SOURCE[0]} or $0,
    # as those will point to the bats executable's location or the preprocessed file respectively
    DIR="$( cd "$( dirname "$BATS_TEST_FILENAME" )" >/dev/null 2>&1 && pwd )"
    TTY_SOCK=$DIR/tty.sock

    cedana debug recvtty "$TTY_SOCK" &
    sleep 1 3>-
}

teardown() {
    sleep 1 3>-
    rm -f $TTY_SOCK
    stop_cedana
    sleep 1 3>-
}

@test "Dump workload without streaming" {
    local task="./workload.sh"
    local job_id="workload-without-stream-1"
    rm -rf /test

    # execute and checkpoint without streaming
    exec_task $task $job_id
    sleep 1 3>-
    checkpoint_task $job_id /test
    [[ "$status" -eq 0 ]]
}

@test "Restore workload without streaming" {
    local task="./workload.sh"
    local job_id="workload-without-stream-2"
    rm -rf /test

    # execute, checkpoint, and restore without streaming
    exec_task $task $job_id
    sleep 1 3>-
    checkpoint_task $job_id /test
    sleep 1 3>-
    run restore_task $job_id
    [[ "$status" -eq 0 ]]
}

@test "Dump workload with streaming" {
    local task="./workload.sh"
    local job_id="workload-stream-1"
    rm -rf /test

    # execute and checkpoint with streaming
    exec_task $task $job_id
    sleep 1 3>-
    checkpoint_task $job_id /test --stream 4
    [[ "$status" -eq 0 ]]
}

@test "Restore workload with streaming" {
    local task="./workload.sh"
    local job_id="workload-stream-2"
    rm -rf /test

    # execute, checkpoint, and restore with streaming
    exec_task $task $job_id
    sleep 1 3>-
    checkpoint_task $job_id /test --stream 8
    sleep 1 3>-
    run restore_task $job_id --stream 8
    [[ "$status" -eq 0 ]]
}
