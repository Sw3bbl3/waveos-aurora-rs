# Shortcuts for `cargo xtask`, which does the work (see xtask/src/main.rs).
# `make` on its own lists them. ARCH=aarch64 (or x86_64) picks the processor
# architecture, e.g. `make run ARCH=aarch64`; see docs/BUILDING.md.

ARCH_FLAG := $(if $(ARCH),--arch $(ARCH))

.DEFAULT_GOAL := help
.PHONY: help run quick build test lint image iso clean

help: ## List these commands
	@echo "WaveOS Aurora"
	@echo
	@awk 'BEGIN {FS = ":.*## "} /^[a-z]+:.*## / {printf "  make %-7s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo
	@echo "Add ARCH=aarch64 for the Arm build. Without make: ./waveos (the launcher) or ./waveos quick."
	@echo "Every option: docs/BUILDING.md"

run: ## Open the launcher: choose displays, memory, devices…, then launch
	cargo xtask launch $(ARCH_FLAG)

quick: ## Boot right away with the configuration you launched last
	cargo xtask launch --last $(ARCH_FLAG)

build: ## Build the bootloader, kernel and apps into target/esp
	cargo xtask build $(ARCH_FLAG)

test: ## Run the kernel self-tests (headless, on a fresh test disk)
	cargo xtask test $(ARCH_FLAG)

lint: ## Clippy on every crate; warnings are errors
	cargo xtask lint $(ARCH_FLAG)

image: ## Build a USB stick image, target/waveos-aurora-usb.img
	cargo xtask image $(ARCH_FLAG)

iso: ## Build a UEFI ISO for VirtualBox and other VMs, target/waveos-aurora.iso
	cargo xtask iso $(ARCH_FLAG)

# Not `cargo clean`: that would also delete target/waveos-aurora.img, the
# disk with everything saved inside WaveOS, and the downloaded Linux packages.
clean: ## Delete compiled output; keeps your WaveOS disk and the downloaded Linux packages
	rm -rf target/debug target/release target/aarch64-apple-darwin target/x86_64-unknown-linux-gnu \
		target/x86_64-unknown-none target/x86_64-unknown-uefi target/x86_64-unknown-linux-musl \
		target/aarch64-unknown-none target/aarch64-unknown-uefi \
		target/user target/esp target/esp-test target/iso-esp target/iso-staging \
		target/user-aarch64 target/esp-aarch64 target/esp-test-aarch64 target/iso-esp-aarch64 target/iso-staging-aarch64
