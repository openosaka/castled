#!/bin/bash

cd "$(dirname "$0")"

go build -o .bin/ ./ping/ping.go
go build -o .bin/file_server ./file_server/main.go

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
