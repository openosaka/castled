#!/bin/bash
# set -x

cargo build
rm -f actual.txt expected.txt || true

data="Hello, Tunneld!"

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill $client_pid
  kill $server_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec ./target/debug/tunneld &
server_pid=$!

# Give the server some time to start
usleep 500000

# Start the tunnel client
exec ./target/debug/tunnel tcp 12345 --remote-port 9992 &
client_pid=$!

# Start the nc TCP server
exec nc -l -p 12345 > actual.txt & # it closes with nc's timeout

sleep 1

echo $data | nc localhost 12345 -q 1 # quit after 1 second
echo $data > expected.txt

diff actual.txt expected.txt
