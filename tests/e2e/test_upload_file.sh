#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

rm -f /tmp/test_file*.txt

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

dd if=/dev/zero of=/tmp/test_file1.txt bs=1K count=2 #(2K)
dd if=/dev/zero of=/tmp/test_file3.txt bs=1M count=100 #(100M)

test() {
  port=$1
  response_code=$(curl -s -o /dev/null -w "%{http_code}" -X POST -F "file=@/tmp/test_file1.txt;filename=/tmp/test_file2.txt" -F "file=@/tmp/test_file3.txt;filename=/tmp/test_file4.txt" "http://localhost:$port/upload?dest=/tmp/test_file2.txt")
  if [ $response_code -eq 200 ]; then
    echo "Response code is 200"
  else
    error "Response code is $response_code"
    exit 1
  fi

  diff /tmp/test_file1.txt /tmp/test_file2.txt
  diff /tmp/test_file3.txt /tmp/test_file4.txt
}

test 6890
test 6891
