# if you want to enable tokio console, you can make TARGET ENABLE_TOKIO_CONSOLE=1
ENABLE_TOKIO_CONSOLE ?= 0
RUST_LOG = INFO

RUSTFLAGS =
FEATURES =

IMAGE_VERSION ?= latest

ifeq ($(ENABLE_TOKIO_CONSOLE), 1)
	RUSTFLAGS +="--cfg tokio_unstable"
	FEATURES += "debug"
endif

PWD := $(shell pwd)
PROTOC_GEN_GO = $(PWD)/.bin/protoc-gen-go
PROTOC_GEN_GO_GRPC = $(PWD)/.bin/protoc-gen-go-grpc
BUF_VERSION = 1.34.0
BUF = $(PWD)/.bin/buf
RUN_BUF = PATH=$(PWD)/.bin:$$PATH $(BUF)
PROTOC_GEN_GO_VERSION := $(shell awk '/google.golang.org\/protobuf/ {print substr($$2, 2)}' go.mod)
# https://pkg.go.dev/google.golang.org/grpc/cmd/protoc-gen-go-grpc
PROTOC_GEN_GO_GRPC_VERSION := 1.4.0

EXAMPLES = \
	crawler

.PHONY: build
build:
	RUSTFLAGS=$(RUSTFLAGS) cargo build $(if $(FEATURES),--features $(FEATURES))

.PHONY: build-docker
build-docker:
	docker build -t castled:$(IMAGE_VERSION) .
	DOCKER_BUILDKIT=1 docker build -t castled:$(IMAGE_VERSION) .
	DOCKER_BUILDKIT=1 docker build -t castle:$(IMAGE_VERSION) .

.PHONY: run-server
run-server: build
	RUST_LOG=$(RUST_LOG) ./target/debug/castled --domain localhost --ip 127.0.0.1

.PHONY: run-client
run-client: build
	TOKIO_CONSOLE_BIND=127.0.0.1:6670 RUST_LOG=$(RUST_LOG) ./target/debug/castle tcp 12345 --remote-port 9991

# the default CLIENT_BINARY is the castle binary
# we can set CLIENT_BINARY to test any language client
CLIENT_BINARY =
E2E_TEST_TCP = 1
E2E_TEST_UDP = 1

.PHONY: e2e
e2e: build
	[ -n "$(CLIENT_BINARY)" ] && export CLIENT_BINARY=$(CLIENT_BINARY) || true
	if [ "$(E2E_TEST_TCP)" = "1" ]; then \
		$(MAKE) e2e-tcp; \
	fi

	if [ "$(E2E_TEST_UDP)" = "1" ]; then \
		$(MAKE) e2e-udp; \
	fi

.PHONY: e2e-tcp
e2e-tcp:
	./tests/e2e/test_close_server_gracefully.sh
	./tests/e2e/test_client_close_when_server_close.sh
	./tests/e2e/test_tcp_local_server_not_start.sh
	./tests/e2e/test_tcp_with_tunnel_http_server.sh
	./tests/e2e/test_tcp_tunnel_to_google_dns.sh
	./tests/e2e/test_http_tunnel_with_domain.sh
	./tests/e2e/test_http_tunnel_with_subdomain.sh
	./tests/e2e/test_http_tunnel_with_given_port.sh
	./tests/e2e/test_upload_file.sh
	./tests/e2e/test_download_file.sh

.PHONY: e2e-udp
e2e-udp:
	./tests/e2e/test_udp_tunnel_to_google_dns.sh
	./tests/e2e/test_udp_reuse_data_streaming.sh

NEW_CRATE_VERSION="0.0.1-alpha.1"

.PHONY: check-version
check-version:
	@VERSION_IN_CARGO=$$(grep -E '^version = ".*"' Cargo.toml | sed -E 's/version = "(.*)"/\1/'); \
	if [ "$${VERSION_IN_CARGO}" != "$(NEW_CRATE_VERSION)" ]; then \
		echo "Error: Version in Cargo.toml ($${VERSION_IN_CARGO}) does not match expected version ($(NEW_CRATE_VERSION))"; \
		exit 1; \
	else \
		echo "Version in Cargo.toml matches expected version ($(NEW_CRATE_VERSION))"; \
	fi

.PHONY: update-deps
update-deps: $(BUF)
	$(RUN_BUF) mod update
	go get -u ./...

.PHONY: generate-proto
generate-proto: $(BUF) $(PROTOC_GEN_GO) $(PROTOC_GEN_GO_GRPC)
	$(RUN_BUF) generate -v --debug

.PHONY: build-examples
build-examples:
	DOCKER_BUILDKIT=1 docker build -t crawler:$(IMAGE_VERSION) -f go.Dockerfile .

.PHONY: test-examples
test-examples: build-examples build-docker
	$(MAKE) -C examples stop
	$(MAKE) -C examples start
	$(MAKE) run-examples

.PHONY: run-examples
run-examples:
	for example in $(EXAMPLES); do \
		$(MAKE) -C examples run-example EXAMPLE=$$example; \
	done

.PHONY: clean
clean:
	rm -rf target

$(BUF):
	mkdir -p .bin
	GOBIN=$(PWD)/.bin go install github.com/bufbuild/buf/cmd/buf@v$(BUF_VERSION)

$(PROTOC_GEN_GO):
	mkdir -p .bin
	GOBIN=$(PWD)/.bin go install google.golang.org/protobuf/cmd/protoc-gen-go@v$(PROTOC_GEN_GO_VERSION)

$(PROTOC_GEN_GO_GRPC):
	mkdir -p .bin
	GOBIN=$(PWD)/.bin go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@v$(PROTOC_GEN_GO_GRPC_VERSION)