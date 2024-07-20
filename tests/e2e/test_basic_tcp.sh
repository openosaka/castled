#!/bin/bash
set -x

rm -f actual.txt expected.txt || true

data="Hello, Castle!"

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $client_pid
  kill -SIGINT $server_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec ./target/debug/castled &
server_pid=$!
sleep 0.2

# Start the tunnel client
exec ./target/debug/castle tcp 12348 --remote-port 9992 &
client_pid=$!

# Start the nc TCP server
exec nc -l -p 12348 > actual.txt & # it closes with nc's timeout
sleep 0.2

# quit after 1 second
# TODO(sword): we don't require this timeout,
# but our server has some bug, so have to quit by timeout now
echo $data | timeout 1s nc localhost 9992 
echo $data > expected.txt

diff actual.txt expected.txt
