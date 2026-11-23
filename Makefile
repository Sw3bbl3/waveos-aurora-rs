# Shortcuts for `cargo xtask`, which does the work (see xtask/src/main.rs).
# `make` on its own lists them.

.DEFAULT_GOAL := help
.PHONY: help run quick build test lint image iso clean

help: ## List these commands
	@echo "WaveOS Aurora"
	@echo
	@awk 'BEGIN {FS = ":.*## "} /^[a-z]+:.*## / {printf "  make %-7s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo
	@echo "Without make: ./waveos (the launcher) or ./waveos quick. Every option: docs/BUILDING.md"

run: ## Open the launcher: choose displays, memory, devices…, then launch
	cargo xtask launch

quick: ## Boot right away with the configuration you launched last
	cargo xtask launch --last

build: ## Build the bootloader, kernel and apps into target/esp
	cargo xtask build

test: ## Run the kernel self-tests (headless, on a fresh test disk)
	cargo xtask test

lint: ## Clippy on every crate; warnings are errors
	cargo xtask lint

image: ## Build a USB stick image, target/waveos-aurora-usb.img
	cargo xtask image

iso: ## Build a UEFI ISO for VirtualBox and other VMs, target/waveos-aurora.iso
	cargo xtask iso

# Not `cargo clean`: that would also delete target/waveos-aurora.img, the
# disk with everything saved inside WaveOS, and the downloaded Linux packages.
clean: ## Delete compiled output; keeps your WaveOS disk and the downloaded Linux packages
	rm -rf target/debug target/release target/aarch64-apple-darwin target/x86_64-unknown-linux-gnu \
		target/x86_64-unknown-none target/x86_64-unknown-uefi target/x86_64-unknown-linux-musl \
		target/user target/esp target/esp-test target/iso-esp target/iso-staging
