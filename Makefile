CARGO ?= cargo
NPM ?= npm
RUNTIME ?= $(shell command -v podman >/dev/null 2>&1 && echo podman || echo docker)
IMAGE ?= localhost/mini-amaru:latest
TARGET ?=
CONFIG ?= projects/amaru.yaml
PROMPT ?= Describe the goal here
GOAL ?= Continue the work of Milestone 5 until it is completed.

.PHONY: help install package image image-check check fmt fmt-check clippy test web web-check doctor run resume status jobs serve loop loop-dry clean

help:
	@echo "Mini-Amaru (Rust)"
	@echo "  make install                         build release binary"
	@echo "  make package                         build installable archive in dist/"
	@echo "  make image                           build the base image (podman or docker)"
	@echo "  make image-check                     build the base image and smoke-test it"
	@echo "  make check                           format check, lint and test"
	@echo "  make web-check                       lint and build the frontend"
	@echo "  make doctor                          check configuration and tools"
	@echo "  make run PROMPT='...'                 run a job"
	@echo "  make resume JOB=<id>                  continue a job"
	@echo "  make status JOB=<id>                  show job status"
	@echo "  make jobs                            list jobs"
	@echo "  make loop-dry GOAL='...'              observe delivery work"
	@echo "  make loop GOAL='...'                  run delivery loop"
	@echo "  make serve                           start local API"

install:
	$(CARGO) build --release --locked

package:
	CARGO="$(CARGO)" NPM="$(NPM)" TARGET="$(TARGET)" ./scripts/package.sh

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
	$(CARGO) clippy --locked --all-targets -- -D warnings

test:
	$(CARGO) test --locked

## Install the frontend dependencies and build it into web/dist.
web:
	cd web && $(NPM) ci && $(NPM) run build

## Run every frontend check that CI runs.
web-check: web
	cd web && $(NPM) run lint

doctor:
	$(CARGO) run --locked -- -c "$(CONFIG)" doctor

run:
	$(CARGO) run --locked -- -c "$(CONFIG)" run -p "$(PROMPT)"

resume:
	$(CARGO) run --locked -- -c "$(CONFIG)" resume --job "$(JOB)"

status:
	$(CARGO) run --locked -- -c "$(CONFIG)" status --job "$(JOB)"

jobs:
	$(CARGO) run --locked -- -c "$(CONFIG)" jobs

loop-dry:
	$(CARGO) run --locked -- -c "$(CONFIG)" loop -p "$(GOAL)" --dry-run

loop:
	$(CARGO) run --locked -- -c "$(CONFIG)" loop -p "$(GOAL)"

serve:
	$(CARGO) run --locked -- -c "$(CONFIG)" serve

clean:
	$(CARGO) clean
