BIN_DIR := $(CURDIR)/.bin

# Determine OS
ifeq ($(shell uname -s),Linux)
DL_OS := linux
DL_OSALT := linux
else
DL_OS := darwin
DL_OSALT := osx
endif
# arch
ifeq ($(shell uname -p),x86_64)
DL_ARCH := amd64
else ifeq ($(shell uname -p),i386)
DL_ARCH := amd64
else
DL_ARCH := arm64
endif

KIND_CLUSTER = castled
## check https://github.com/kubernetes-sigs/kind/releases
KIND_VERSION := v0.23.0
KIND := $(BIN_DIR)/kind
GATEWAYAPI_VERSION=v1.1.0
## check https://hub.docker.com/r/kindest/node/tags
KIND_NODE_IMAGE := kindest/node:v1.30.0

## check https://kubernetes.io/releases/
KUBECTL_VERSION := v1.30.1
KUBECTL := $(BIN_DIR)/kubectl
RUN_KUBECTL := $(KUBECTL) --context kind-$(KIND_CLUSTER)

ISTIO_VERSION := 1.22.3
ISTIOCTL := $(BIN_DIR)/istioctl

IMAGE_TAG := latest
IMAGES = \
	castled \
	castle \
	crawler

start: $(KIND) $(KUBECTL) $(ISTIOCTL)
	$(KIND) create cluster --name $(KIND_CLUSTER) --image=$(KIND_NODE_IMAGE)
	$(RUN_KUBECTL) kustomize "github.com/kubernetes-sigs/gateway-api/config/crd?ref=$(GATEWAYAPI_VERSION)" | $(RUN_KUBECTL) apply -f -
	$(ISTIOCTL) install -y --set profile=minimal
	$(KIND) load docker-image --name $(KIND_CLUSTER) $(patsubst %,%:$(IMAGE_TAG),$(IMAGES))
	$(MAKE) apply

apply:
	$(RUN_KUBECTL) apply -f $(CURDIR)/manifests

stop: $(KIND)
	$(KIND) delete cluster --name $(KIND_CLUSTER)

$(KIND):
	mkdir -p $(BIN_DIR)
	curl -sSLf -o $@ https://github.com/kubernetes-sigs/kind/releases/download/$(KIND_VERSION)/kind-$(DL_OS)-$(DL_ARCH)
	chmod a+x $@

$(KUBECTL):
	mkdir -p $(BIN_DIR)
	curl -sSLf -o $@ "https://dl.k8s.io/release/$(KUBECTL_VERSION)/bin/$(DL_OS)/$(DL_ARCH)/kubectl"
	chmod a+x $@

$(ISTIOCTL):
	mkdir -p $(BIN_DIR)
	curl -sSLf https://github.com/istio/istio/releases/download/$(ISTIO_VERSION)/istioctl-$(ISTIO_VERSION)-$(DL_OSALT)-$(DL_ARCH).tar.gz \
	| tar -C $(BIN_DIR) -xzf -