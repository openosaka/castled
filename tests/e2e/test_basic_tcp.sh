#!/bin/bash
# set -x

cargo build
rm -f actual.txt expected.txt || true

data="Hello, Tunneld!"

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $client_pid
  kill -SIGINT $server_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec ./target/debug/tunneld &
server_pid=$!

# Give the server some time to start
sleep 1

# Start the tunnel client
exec ./target/debug/tunnel tcp 12345 --remote-port 9992 &
client_pid=$!

# Start the nc TCP server
exec nc -l -p 12345 > actual.txt & # it closes with nc's timeout

sleep 1

# quit after 1 second
# TODO(sword): we don't require this timeout,
# but our server has some bug, so have to quit by timeout now
echo $data | timeout 1s nc localhost 9992 
echo $data > expected.txt

diff actual.txt expected.txt
