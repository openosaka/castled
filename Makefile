# if you want to enable tokio console, you can make TARGET ENABLE_TOKIO_CONSOLE=1
ENABLE_TOKIO_CONSOLE ?= 0

RUSTFLAGS =
FEATURES =

ifeq ($(ENABLE_TOKIO_CONSOLE), 1)
	RUSTFLAGS +="--cfg tokio_unstable"
	FEATURES += "otel"
endif

.PHONY: build
build:
	RUSTFLAGS=$(RUSTFLAGS) cargo build $(if $(FEATURES),--features $(FEATURES))

.PHONY: run-server
run-server: build
	RUST_LOG=INFO ./target/debug/tunneld

.PHONY: run-client
run-client: build
	TOKIO_CONSOLE_BIND=127.0.0.1:6670 RUST_LOG=debug ./target/debug/tunnel tcp 12345 --remote-port 9991

.PHONY: e2e
e2e: build
	./tests/e2e/test_close_server_gracefully.sh
	./tests/e2e/test_basic_tcp.sh
	./tests/e2e/test_tcp_local_server_not_start.sh
	./tests/e2e/test_tcp_with_tunnel_http_server.sh
	./tests/e2e/test_tcp_tunnel_to_google_dns.sh
	./tests/e2e/test_http_tunnel_with_domain.sh
	./tests/e2e/test_http_tunnel_with_subdomain.sh
	./tests/e2e/test_http_tunnel_with_given_port.sh
