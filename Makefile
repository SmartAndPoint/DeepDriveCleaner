# Homebrew's Rust 1.81 sits before rustup in PATH on the reference machine and
# breaks both `cargo` and `clippy-driver`; force the rustup toolchain.
CARGO := $(HOME)/.cargo/bin/cargo
export PATH := $(HOME)/.cargo/bin:$(PATH)

.PHONY: build fmt fmt-check clippy test check release install dist

build:
	$(CARGO) build --workspace

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

check: fmt-check clippy test

release:
	$(CARGO) build --workspace --release

# Installs `deepdrivecleaner` into ~/.cargo/bin and `ddc` as a symlink to it
# (one binary, so macOS privacy grants such as Full Disk Access apply to both).
install:
	$(CARGO) install --path crates/ddc-cli --locked
	ln -sf "$(HOME)/.cargo/bin/deepdrivecleaner" "$(HOME)/.cargo/bin/ddc"

# Signed, notarized release tarball in dist/ (needs .env with DDC_SIGN_IDENTITY and
# NOTARY_PROFILE); publish it with scripts/publish-release.sh.
dist: check
	./scripts/build-dist.sh
