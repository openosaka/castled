#!/bin/bash

cd "$(dirname "$0")"

go build -o .bin/ ./ping/ping.go
go build -o .bin/file_server ./file_server/main.go
go build -o .bin/castlego ./castle_go/main.go

wait_port() {
    local port=$1
    local timeout=${2:-5}  # Default timeout is 5 seconds
    local host="127.0.0.1"

    for ((i=0; i<timeout*10; i++)); do
        if nc -z "$host" "$port"; then
            return 0
        fi
        sleep 0.5
    done

    echo "Port $port is not available after ${timeout} seconds."
    exit 1
}

wait_port_on_udp() {
    local port=$1
    local timeout=${2:-5}  # Default timeout is 5 seconds
    local host="127.0.0.1"

    for ((i=0; i<timeout*10; i++)); do
        if nc -uz "$host" "$port"; then
            return 0
        fi
        sleep 0.5
    done

    echo "Port $port is not available after ${timeout} seconds."
    exit 1
}

default_client_binary=$root_dir/target/debug/castle

run_client() {
    # if pass env CLIENT_BINARY, then use it.
    # othsewise, use default_client_binary
    local client_binary=${CLIENT_BINARY:-$default_client_binary}
    exec $client_binary "$@" &
}

run_client_block() {
    # if pass env CLIENT_BINARY, then use it.
    # othsewise, use default_client_binary
    local client_binary=${CLIENT_BINARY:-$default_client_binary}
    $client_binary "$@"
}

# print red color
error() {
    echo -e "\033[0;31m$1\033[0m"
}
