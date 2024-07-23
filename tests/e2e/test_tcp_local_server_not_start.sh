#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $client_pid
  kill -SIGINT $server_pid
}

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

# Start the tunnel client
run_client tcp 12345 --remote-port 9992
client_pid=$!
wait_port 9992

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
