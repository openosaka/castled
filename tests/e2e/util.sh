#!/bin/bash

cd "$(dirname "$0")"

go build -o .bin/ ./ping/ping.go
go build -o .bin/castlego ./castle_go/main.go
go build -o .bin/file_server ./file_server/main.go
go build -o .bin/udprint ./udprint/main.go

wait_port() {
    local port=$1
    local timeout=${2:-5}  # Default timeout is 5 seconds
    local host="127.0.0.1"

    for ((i=0; i<timeout*10; i++)); do
        if netstat -tuln | grep -q ":$port"; then
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
