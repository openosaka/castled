.PHONY: build
build:
	RUSTFLAGS="--cfg tokio_unstable" cargo build

.PHONY: run-server
run-server: build
	RUST_LOG=debug ./target/debug/tunneld

.PHONY: run-client
run-client: build
	TOKIO_CONSOLE_BIND=127.0.0.1:6670 RUST_LOG=debug ./target/debug/tunnel tcp 12345 --remote-port 9991
