#!/bin/bash

wait_port() {
    local port=$1
    local timeout=${2:-5}  # Default timeout is 5 seconds
    local host="127.0.0.1"

    for ((i=0; i<timeout*10; i++)); do
        if nc -z "$host" "$port"; then
            return 0
        fi
        sleep 0.1
    done

    echo "Port $port is not available after ${timeout} seconds."
    exit 1
}

RUSTFLAGS="--cfg tokio_unstable" cargo build 
