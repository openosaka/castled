#!/bin/bash
# set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $server_pid # close server at first, if something blocked, then this script is not ganna finished.
  kill -SIGINT $client_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec ./target/debug/castled &
server_pid=$!

# Give the server some time to start
sleep 1

# Start the tunnel client
exec ./target/debug/castle tcp 12345 --remote-port 9992 &
client_pid=$!

sleep 1
