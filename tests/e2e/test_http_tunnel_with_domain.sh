#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

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

wait_port 6610

# Start the tunnel client
exec $root_dir/target/debug/tunnel http 12345 --domain "foo.com" &
client_pid=$!

sleep 1

# test can't bind to the same subdomain
$root_dir/target/debug/tunnel http 6666 --domain "foo.com"
error_code=$?
echo $error_code
if [[ $error_code -eq 0 ]]; then
	echo "Test failed: Expected non-zero error code, got $error_code"
	exit 1
fi

# Start the nc TCP server
exec $cur_dir/ping.py 12345 &
http_server_pid1=$!

wait_port 12345

response=$(curl -s -H "Host: foo.com" http://localhost:6611/ping?query=tunneld)
if [[ $response != "pong=tunneld" ]]; then
	echo "Test failed: Response is not pong=tunneld, is $response"
	exit 1
fi

# kill the client and register again
kill -SIGINT $client_pid
sleep 1
exec $root_dir/target/debug/tunnel http 12345 --domain foo.com &
client_pid=$!

exec $root_dir/target/debug/tunnel http 12346 --domain bar.com &
client_pid2=$!
exec $cur_dir/ping.py 12346 &
http_server_pid2=$!

wait_port 12346

response=$(curl -s -H "Host: foo.com" http://localhost:6611/ping?query=server1)
if [[ $response != "pong=server1" ]]; then
	echo "Test failed: Response is not pong=server1, is $response"
	exit 2
fi

response=$(curl -s -H "Host: bar.com" http://localhost:6611/ping?query=server2)
if [[ $response != "pong=server2" ]]; then
	echo "Test failed: Response is not pong=server1, is $response"
	exit 3
fi
