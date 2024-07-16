#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

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
exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

# Start the tunnel client
exec $root_dir/target/debug/castle http 13346 --subdomain foo &
client_pid=$!

sleep 0.5

# test can't bind to the same subdomain
$root_dir/target/debug/castle http 6666 --subdomain foo
error_code=$?
if [[ $error_code -eq 0 ]]; then
	echo "Test failed: Expected non-zero error code, got $error_code"
	exit 1
fi

# Start the nc TCP server
exec $cur_dir/ping.py 13346 &
http_server_pid1=$!
wait_port 13346

response=$(curl -s -H "Host: foo.example" http://localhost:6611/ping?query=castled)
if [[ $response != "pong=castled" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi

# kill the client and register again
kill -SIGINT $client_pid
sleep 0.5
exec $root_dir/target/debug/castle http 13346 --subdomain foo &
client_pid=$!

exec $root_dir/target/debug/castle http 13347 --subdomain bar &
client_pid2=$!
exec $cur_dir/ping.py 13347 &
http_server_pid2=$!

wait_port 13347

response=$(curl -s -H "Host: foo.example" http://localhost:6611/ping?query=server1)
if [[ $response != "pong=server1" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi

response=$(curl -s -H "Host: bar.example" http://localhost:6611/ping?query=server2)
if [[ $response != "pong=server2" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi
