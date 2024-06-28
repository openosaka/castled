#!/bin/bash
set -x

cargo build

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
exec ./target/debug/tunneld &
server_pid=$!

# Give the server some time to start
sleep 1

# Start the tunnel client
exec ./target/debug/tunnel tcp 8881 --remote-port 9992 &
client_pid=$!

# Start the nc TCP server
exec python3 -m http.server 8881 > /dev/null 2>&1 &
http_server_pid=$!

sleep 1

for i in {1..10000}
do
  curl -s http://localhost:9992 > /dev/null 2>&1
  if [ $? -ne 0 ]; then
    echo "Failed to connect to the tunnel server"
    exit 1
  fi
done
