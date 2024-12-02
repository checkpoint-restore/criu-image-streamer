#!/bin/bash


## NOTE: All scripts are being run by the makefile, which runs in the scripts/ci directory.
## As a result, where these functions are called rely on managing directory state using pushd/popd,
## which also means all these functions assume they're being run in the root directory.
## Look at regression-test main for an example.
##

print_header() {
    echo "############### $1 ###############"
}

print_env() {
    set +x
    print_header "Environment variables"
    printenv
    print_header "uname -a"
    uname -a || :
    print_header "Mounted file systems"
    cat /proc/self/mountinfo || :
    print_header "Kernel command line"
    cat /proc/cmdline || :
    print_header "Kernel modules"
    lsmod || :
    print_header "Distribution information"
    [ -e /etc/lsb-release ] && cat /etc/lsb-release
    [ -e /etc/redhat-release ] && cat /etc/redhat-release
    [ -e /etc/alpine-release ] && cat /etc/alpine-release
    print_header "ulimit -a"
    ulimit -a
    print_header "Available memory"
    if [ -e /etc/alpine-release ]; then
        # Alpine's busybox based free does not understand -h
        free
    else
        free -h
    fi
    print_header "Available CPUs"
    lscpu || :
    set -x
}

source_env() {
    source /etc/environment
}

start_cedana() {
    ./build-start-daemon.sh --no-build
}

stop_cedana() {
    ./reset.sh
}
