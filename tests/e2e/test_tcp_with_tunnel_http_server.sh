#!/bin/bash

set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $server_pid
  kill -SIGINT $client_pid
  kill -SIGINT $http_server_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

# Start the tunnel client
exec $root_dir/target/debug/castle tcp 8881 --remote-port 9992 &
client_pid=$!
wait_port 9992

# Start the nc TCP server
exec python3 -m http.server 8881 > /dev/null 2>&1 &
http_server_pid=$!
wait_port 8881

for i in {1..200}
do
  curl -s http://localhost:9992 > /dev/null 2>&1
  if [ $? -ne 0 ]; then
    error "Failed to connect to the castle server"
    exit 1
  fi
done
