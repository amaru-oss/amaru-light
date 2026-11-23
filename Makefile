CARGO ?= cargo
NPM ?= npm
PODMAN ?= podman
IMAGE ?= localhost/mini-amaru:latest
TARGET ?=
CONFIG ?= projects/amaru.yaml
PROMPT ?= Describe the goal here
GOAL ?= Continue the work of Milestone 5 until it is completed.

.PHONY: help install package image check doctor run resume status jobs serve loop loop-dry clean

help:
	@echo "Mini-Amaru (Rust)"
	@echo "  make install                         build release binary"
	@echo "  make package                         build installable archive in dist/"
	@echo "  make image                           build the Podman image"
	@echo "  make check                           format, lint and test"
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

image:
	$(PODMAN) build -f Containerfile -t "$(IMAGE)" .

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --all-targets -- -D warnings
	$(CARGO) test --locked

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
