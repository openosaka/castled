.PHONY: build
build:
	cargo build

.PHONY: server
server: build
	RUST_LOG=debug ./target/debug/tunneld

.PHONY: client
client: build
	RUST_LOG=debug ./target/debug/tunnel tcp 12345 --remote-port 9991
