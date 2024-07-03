#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e

cargo build

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $client_pid
  kill -SIGINT $client_pid2
  kill -SIGINT $server_pid
  kill $http_server_pid1
  kill $http_server_pid2
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec $root_dir/target/debug/tunneld &
server_pid=$!

# Give the server some time to start
sleep 1

# Start the tunnel client
exec $root_dir/target/debug/tunnel http 12345 --subdomain foo &
client_pid=$!

# Start the nc TCP server
exec $cur_dir/ping.py 12345 &
http_server_pid1=$!

sleep 1
response=$(curl -s -H "Host: foo.example" http://localhost:12345/ping?query=tunneld)
if [[ $response != "pong=tunneld" ]]; then
	echo "Test failed: Response is not pong=tunneld"
	exit 1
fi

# kill the client and register again
kill -SIGINT $client_pid
exec $root_dir/target/debug/tunnel http 12345 --subdomain foo &
client_pid=$!

exec $root_dir/target/debug/tunnel http 12346 --subdomain bar &
client_pid2=$!
exec $cur_dir/ping.py 12346 &
http_server_pid2=$!

response=$(curl -s -H "Host: foo.example" http://localhost:12345/ping?query=server1)
if [[ $response != "pong=server1" ]]; then
	echo "Test failed: Response is not pong=tunneld"
	exit 1
fi

response=$(curl -s -H "Host: bar.example" http://localhost:12345/ping?query=server2)
if [[ $response != "pong=server2" ]]; then
	echo "Test failed: Response is not pong=tunneld"
	exit 1
fi
