#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

rm -f /tmp/download_file*.txt

cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $server_pid
  kill -SIGINT $client_pid1
  kill -SIGINT $client_pid2
  kill $file_server_pid
}

trap cleanup EXIT

exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

run_client http 13346 --remote-port 6890
client_pid1=$!
wait_port 6890

run_client tcp 13346 --remote-port 6891
client_pid2=$!
wait_port 6891

RUST_LOG=INFO exec $cur_dir/.bin/file_server --port 13346 &
file_server_pid=$!

dd if=/dev/zero of=/tmp/download_file1.txt bs=1K count=2 #(2K)
dd if=/dev/zero of=/tmp/download_file3.txt bs=1M count=1 #(1G)

test() {
  port=$1
  response_code=$(curl -s -o /tmp/download_file2.txt -w "%{http_code}" -X POST "http://localhost:$port/download?file=/tmp/download_file1.txt")
  if [ $response_code -eq 200 ]; then
    echo "Response code is 200"
  else
    error "Response code is $response_code"
    exit 1
  fi

  response_code=$(curl -s -o /tmp/download_file4.txt -w "%{http_code}" -X POST "http://localhost:$port/download?file=/tmp/download_file3.txt")
  if [ $response_code -eq 200 ]; then
    echo "Response code is 200"
  else
    error "Response code is $response_code"
    exit 1
  fi

  diff /tmp/download_file1.txt /tmp/download_file2.txt
  diff /tmp/download_file3.txt /tmp/download_file4.txt
}

test 6890
test 6891
