#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

# Start the tunnel server
exec $root_dir/target/debug/castled &
server_pid=$!
sleep 1

run_client tcp 12348 --remote-port 9992
client_pid=$!

sleep 1

kill -SIGINT $server_pid

# Wait for the client process to exit
wait $client_pid

# Check the exit code should be 1
if [ $? -ne 1 ]; then
	echo "Test failed"
	exit 1
fi
