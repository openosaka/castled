#!/bin/bash

set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

# Function to clean up processes
cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $server_pid
  kill -SIGINT $client_pid
  kill -SIGINT $udprint_pid
}

rm udprint.txt

# Trap EXIT signal to ensure cleanup
trap cleanup EXIT

# Start the tunnel server
exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

run_client udp 12345 --remote-port 12346
client_pid=$!
wait_port 12346

exec $cur_dir/.bin/udprint > udprint.txt &
udprint_pid=$!
wait_port 12345

for i in {1..5}
do
  echo "hello" | socat - UDP4-SENDTO:127.0.0.1:12346,bind=127.0.0.1:12333
done

# assert there are 5 same messages in udprint.txt
if [ $(cat udprint.txt | grep -c "hello") -eq 5 ]; then
  echo "Test passed: 5 same messages found in udprint.txt"
else
  error "Test failed: 5 same messages not found in udprint.txt"
  exit 1
fi
