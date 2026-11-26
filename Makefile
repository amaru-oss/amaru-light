CARGO ?= cargo
RUNTIME ?= $(shell command -v podman >/dev/null 2>&1 && echo podman || echo docker)
IMAGE ?= localhost/amaru-light:latest
TARGET ?=
CONFIG ?= amaru.yaml

.PHONY: help install package image image-check check fmt fmt-check clippy test doctor serve status clean

help:
	@echo "Amaru Light"
	@echo "  make install                 build the release binary"
	@echo "  make package                 build an installable archive in dist/"
	@echo "  make image                   build the base image (podman or docker)"
	@echo "  make image-check             build the base image and smoke-test it"
	@echo "  make check                   format check, lint and test"
	@echo "  make doctor CONFIG=...       check the tools and credentials"
	@echo "  make serve CONFIG=...        run the daemon"
	@echo "  make status CONFIG=...       list recent tickets"

install:
	$(CARGO) build --release --locked

package:
	CARGO="$(CARGO)" TARGET="$(TARGET)" ./scripts/package.sh

## Build the base image.
image:
	$(RUNTIME) build -f Containerfile -t "$(IMAGE)" .

## Build the base image and run the smoke test in it.
image-check: image
	RUNTIME="$(RUNTIME)" ./scripts/test-image.sh "$(IMAGE)"

## Run every Rust quality gate that CI runs.
check: fmt-check clippy test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --locked --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --locked --workspace

doctor:
	$(CARGO) run --locked --bin amaru -- -c "$(CONFIG)" doctor

serve:
	$(CARGO) run --locked --bin amaru -- -c "$(CONFIG)" serve

status:
	$(CARGO) run --locked --bin amaru -- -c "$(CONFIG)" status

clean:
	$(CARGO) clean
