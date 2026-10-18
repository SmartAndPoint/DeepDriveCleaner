# Homebrew's Rust 1.81 sits before rustup in PATH on the reference machine and
# breaks both `cargo` and `clippy-driver`; force the rustup toolchain.
CARGO := $(HOME)/.cargo/bin/cargo
export PATH := $(HOME)/.cargo/bin:$(PATH)

.PHONY: build fmt fmt-check clippy test check release install dist app app-test app-dist icons

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

# The menu bar app, Debug, signed with the Developer ID (needs DDC_SIGN_IDENTITY in
# .env): build/Debug/DeepDriveCleaner.app with the release CLI in Contents/Helpers.
app:
	./scripts/build-app.sh

# The app's logic (DDCKit) runs under `swift test`, with no app host.
app-test:
	cd app/Packages/DDCKit && swift test

# Notarized, stapled app in dist/DeepDriveCleaner-<version>.zip. Run after `make dist`,
# which empties dist/ first.
app-dist: check app-test
	./scripts/build-app.sh --dist

# Regenerates the AppIcon set from the status item's glyph (DriveGlyph.swift).
icons:
	mkdir -p build
	swiftc -O -o build/icons app/DeepDriveCleaner/DriveGlyph.swift scripts/icons/main.swift
	build/icons app/DeepDriveCleaner/Assets.xcassets/AppIcon.appiconset
