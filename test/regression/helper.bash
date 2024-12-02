#!/usr/bin/env bash

function start_cedana() {
    sudo -E cedana daemon start 2>&1 | sudo tee -a /var/log/cedana-daemon.log &
}

function stop_cedana() {
    sudo pkill cedana
    sleep 2 3>-
    sudo pkill -SIGKILL cedana # killswitch in case it's not stopped
}

function exec_task() {
    local task="$1"
    local job_id="$2"
    shift 2
    cedana exec -w "$DIR" "$task" -i "$job_id" $@
}

function checkpoint_task() {
    local job_id="$1"
    local dir="$2"
    shift 2
    cedana dump job "$job_id" -d "$dir" $@
}

function restore_task() {
    local job_id="$1"
    shift 1
    cedana restore job "$job_id" $@
}

function fail() {
    echo "$@" >&2
    exit 1
}
