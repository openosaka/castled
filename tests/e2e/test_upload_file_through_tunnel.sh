#!/bin/bash
set -x

root_dir=$(git rev-parse --show-toplevel)
cur_dir=$root_dir/tests/e2e
source $cur_dir/util.sh

rm -f /tmp/test_file*.txt

cleanup() {
  echo "Cleaning up..."
  kill -SIGINT $server_pid
  kill -SIGINT $client_pid
  kill $file_server_pid
}

trap cleanup EXIT

exec $root_dir/target/debug/castled &
server_pid=$!
wait_port 6610

RUST_LOG=DEBUG exec $root_dir/target/debug/castle http 13346 --remote-port 6890 &
client_pid=$!
wait_port 6890

RUST_LOG=DEBUG exec $cur_dir/.bin/file_server --port 13346 &
file_server_pid=$!

dd if=/dev/zero of=/tmp/test_file1.txt bs=1K count=2 #(2K)

response_code=$(curl -s -o /dev/null -w "%{http_code}" -X POST -F "file=@/tmp/test_file1.txt" http://localhost:6890/upload?dest=/tmp/test_file2.txt)
if [ $response_code -eq 200 ]; then
  echo "Response code is 200"
else
  echo "Response code is $response_code"
  exit 1
fi

# Small file
# check the size of the test_file2
file_size=$(stat -c %s /tmp/test_file2.txt)
if [ $file_size -ne 2048 ]; then
  echo "file size is not correct"
  exit 1
fi

# check the content of the test_file1 and test_file2
diff /tmp/test_file1.txt /tmp/test_file2.txt

# Large file
dd if=/dev/zero of=/tmp/test_file3.txt bs=1G count=2 #(2G)

curl -X POST -F "file=@/tmp/test_file3.txt" http://localhost:6890/upload?dest=/tmp/test_file4.txt

# check the size of the test_file2
file_size=$(stat -c %s /tmp/test_file4.txt)
if [ $file_size -ne 2147483648 ]; then
  echo "file size is not correct"
  exit 1
fi

diff /tmp/test_file3.txt /tmp/test_file4.txt
