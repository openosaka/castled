#!/bin/bash
# set -x

cargo build

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
sleep 1

# Start the tunnel client
exec ./target/debug/tunnel tcp 12345 --remote-port 9992 &
client_pid=$!

start=$(date +%s.%N)
curl http://localhost:9992 --max-time 3 || true # should failed immediately
end=$(date +%s.%N)
duration=$(echo "$end - $start" | bc)

threshold=1.0
if (( $(echo "$duration < $threshold" | bc -l) )); then
	exit 0
else
	exit 1
fi
