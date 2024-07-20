#!/bin/bash
set -x

# Start the tunnel server
exec ./target/debug/castled &
server_pid=$!
sleep 1

exec ./target/debug/castle tcp 12348 --remote-port 9992 &
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
