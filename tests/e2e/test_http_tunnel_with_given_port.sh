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
exec $root_dir/target/debug/castle http 6666 --remote-port 16666 &
client_pid=$!
wait_port 16666

# test can't bind to the same remote port
$root_dir/target/debug/castle http 6666 --remote-port 16666
error_code=$?
if [[ $error_code -eq 0 ]]; then
	echo "Test failed: Expected non-zero error code, got $error_code"
	exit 1
fi

# Start the nc TCP server
exec $cur_dir/.bin/ping --port 6666 &
http_server_pid1=$!
wait_port 6666

response=$(curl -s http://localhost:16666/ping?query=castled)
if [[ $response != "pong=castled" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi

# kill the client and register again
kill -SIGINT $client_pid
sleep 0.5
exec $root_dir/target/debug/castle http 6666 --remote-port 16666 &
client_pid=$!
wait_port 16666

exec $root_dir/target/debug/castle http 6667 --remote-port 16667 &
client_pid2=$!
wait_port 16667
exec $cur_dir/.bin/ping --port 6667 &
http_server_pid2=$!
wait_port 6667

response=$(curl -s http://localhost:16666/ping?query=server1)
if [[ $response != "pong=server1" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi

response=$(curl -s http://localhost:16667/ping?query=server2)
if [[ $response != "pong=server2" ]]; then
	echo "Test failed: Response is not pong=castled"
	exit 1
fi
