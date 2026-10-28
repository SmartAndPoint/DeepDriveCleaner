# Homebrew's Rust 1.81 sits before rustup in PATH on the reference machine and
# breaks both `cargo` and `clippy-driver`; force the rustup toolchain. Where rustup
# keeps no ~/.cargo/bin/cargo (GitHub's macOS image installs it with Homebrew), the
# cargo in PATH is rustup's.
CARGO := $(firstword $(wildcard $(HOME)/.cargo/bin/cargo) cargo)
export PATH := $(HOME)/.cargo/bin:$(PATH)

include .config/tools.mk

# Steps run one at a time, in the order listed, even under `make -j`.
.NOTPARALLEL:

# $(call install-tool,<name>,<version>): cargo-<name> at exactly that version.
install-tool = $(CARGO) $(1) --version 2>/dev/null | grep -qw '$(2)' \
	|| $(CARGO) install --locked --force cargo-$(1)@$(2)

# The gate builds with the complexity counters compiled in, so their tests run too.
# Not --all-features: op-stats and dhat-heap refuse to coexist.
FEATURES := --features ddc-core/op-stats
# The declared minimum Rust version, read from Cargo.toml so it is set in one place.
MSRV := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)

.PHONY: build fmt fmt-check clippy test check release install dist app app-test app-dist icons \
	gate-fast gate nextest doctest clippy-dhat doc deny machete msrv release-check \
	tools require-tools hooks coverage ci-green golden-check golden-record loadgen-clean load-image load leaks asan

build:
	$(CARGO) build --workspace

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets $(FEATURES) --locked -- -D warnings

test:
	$(CARGO) test --workspace

# The mandatory gate (story "Mandatory gate before every commit", DDC-005). Each step
# is its own target, so a failure names the step. gate-fast runs before every commit
# (.githooks/pre-commit), gate before every push (.githooks/pre-push), in CI and in
# `make dist`.
gate-fast: require-tools fmt-check clippy nextest doctest

# deny and machete read only metadata and take seconds: they go first.
gate: require-tools deny machete gate-fast clippy-dhat doc msrv release-check

# Kept for everything that already calls it.
check: gate-fast

nextest:
	$(CARGO) nextest run --workspace $(FEATURES) --locked

# Nextest does not run doc tests.
doctest:
	$(CARGO) test --doc --workspace $(FEATURES) --locked

clippy-dhat:
	$(CARGO) clippy --workspace --all-targets --features ddc-core/dhat-heap --locked -- -D warnings

doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps $(FEATURES) --locked

deny:
	$(CARGO) deny check

machete:
	$(CARGO) machete

msrv:
	$(CARGO) +$(MSRV) check --workspace --all-targets $(FEATURES) --locked

release-check:
	$(CARGO) build --workspace --release --locked

# Golden outputs of the S load trees (bench/golden/, story "Synthetic load trees:
# large, wide, deep and hostile"). The check is part of `nextest`, so the gate runs
# it; golden-record accepts a deliberate change. Review the diff before committing it.
golden-check:
	$(CARGO) nextest run -p ddc-cli --test golden_cli --locked

golden-record:
	DDC_UPDATE_GOLDEN=1 $(CARGO) nextest run -p ddc-cli --test golden_cli --locked

# Load trees past S on a disposable disk image, each scanned and checked against its
# manifest: `make load-image LOAD_PROFILE=mixed LOAD_SIZE=L` (LOAD_PROFILE=all for every
# profile; LOAD_SIZE is S, M, L or XL). CI runs every profile at M; the nightly run,
# mixed at L.
LOAD_PROFILE ?= mixed
LOAD_SIZE ?= M
LOAD_SEED ?= 1
load-image:
	$(CARGO) build --release -p ddc-loadgen --locked
	LOADGEN=target/release/ddc-loadgen scripts/load-image.sh $(LOAD_PROFILE) $(LOAD_SIZE) $(LOAD_SEED)

# The release CLI measured on every profile's tree (story "Memory and CPU under load:
# peak memory, leaks and CPU time"): wall, user and system time, peak memory and
# threads per command, in target/load/load-$(LOAD_SIZE).tsv. LOAD_PROFILE defaults to
# all here. Limits come later: every run records, whatever LOAD_LIMITS says.
LOAD_REPEAT ?= 3
LOAD_LIMITS ?= record
load:
	@[ "$(LOAD_LIMITS)" = record ] || echo "make load: no limits yet; LOAD_LIMITS=$(LOAD_LIMITS) only records" >&2
	$(CARGO) build --release -p ddc-loadgen -p ddc-cli --locked
	LOADGEN=target/release/ddc-loadgen LOAD_CLI=target/release/deepdrivecleaner LOAD_REPEAT=$(LOAD_REPEAT) \
	  LOAD_DATE=$$(date -u +%F) LOAD_COMMIT=$$(git describe --always --dirty) \
	  scripts/load-image.sh $(if $(filter command line environment,$(origin LOAD_PROFILE)),$(LOAD_PROFILE),all) \
	  $(LOAD_SIZE) $(LOAD_SEED)

# `scan --json` of every profile's S tree under `leaks -atExit`, on the cargo release
# build (the notarized app's hardened runtime keeps `leaks` out): fails on any leak.
# Outputs in target/load/leaks-<profile>.txt and .stderr.txt. It joins `make gate`
# after the owner's first run on the reference machine.
leaks:
	$(CARGO) build --release -p ddc-loadgen -p ddc-cli --locked
	target/release/ddc-loadgen leaks --cli target/release/deepdrivecleaner

# AddressSanitizer on the code that holds `unsafe` (the sys:: unit tests) and on a
# scan of every profile's S tree in process (the generator's profile and hostile
# tests), on a dated nightly. LeakSanitizer does not run on macOS; the explicit
# --target keeps proc-macros uninstrumented. The nightly workflow runs it.
ASAN_TOOLCHAIN ?= nightly-2026-10-20
asan:
	rustup toolchain install $(ASAN_TOOLCHAIN) --profile minimal
	RUSTFLAGS=-Zsanitizer=address ASAN_OPTIONS=detect_leaks=0 CARGO_TARGET_DIR=target/asan \
	  $(CARGO) +$(ASAN_TOOLCHAIN) test --locked --target aarch64-apple-darwin -p ddc-core --lib sys::
	RUSTFLAGS=-Zsanitizer=address ASAN_OPTIONS=detect_leaks=0 CARGO_TARGET_DIR=target/asan \
	  $(CARGO) +$(ASAN_TOOLCHAIN) test --locked --target aarch64-apple-darwin -p ddc-loadgen --test profiles --test hostile

# Removes the load trees `ddc-loadgen mktemp` left in $$TMPDIR (only marked ones).
loadgen-clean:
	$(CARGO) run -q -p ddc-loadgen --locked -- clean

# Installs the pinned gate tools (.config/tools.mk) and the MSRV toolchain. A tool
# already at its pinned version is kept. The version is read from the binary, not from
# cargo's install records, which CI's tool cache does not restore.
tools:
	$(call install-tool,nextest,$(NEXTEST_VERSION))
	$(call install-tool,deny,$(DENY_VERSION))
	$(call install-tool,machete,$(MACHETE_VERSION))
	$(call install-tool,llvm-cov,$(LLVM_COV_VERSION))
	rustup component add llvm-tools-preview
	rustup toolchain install $(MSRV) --profile minimal

require-tools:
	@for t in nextest deny machete llvm-cov; do \
		$(CARGO) $$t --version >/dev/null 2>&1 || { echo "cargo-$$t is missing: run make tools" >&2; exit 1; }; \
	done

# Once per clone: git runs .githooks/pre-commit and pre-push.
hooks:
	git config core.hooksPath .githooks

# Line and region coverage under target/, so the tree stays clean for build-dist.sh.
coverage:
	$(CARGO) llvm-cov nextest --workspace $(FEATURES) --locked --lcov --output-path target/lcov.info
	$(CARGO) llvm-cov report --summary-only

# Refuses unless the Gate workflow passed on GitHub for HEAD.
ci-green:
	./scripts/ci-green.sh

release:
	$(CARGO) build --workspace --release

# Installs `deepdrivecleaner` into ~/.cargo/bin and `ddc` as a symlink to it
# (one binary, so macOS privacy grants such as Full Disk Access apply to both).
install:
	$(CARGO) install --path crates/ddc-cli --locked
	ln -sf "$(HOME)/.cargo/bin/deepdrivecleaner" "$(HOME)/.cargo/bin/ddc"

# Both release assets in dist/, from one commit (needs .env with DDC_SIGN_IDENTITY and
# NOTARY_PROFILE): the signed, notarized CLI tarball, then the notarized, stapled app
# zip. Publish them with scripts/publish-release.sh. Only from a commit whose CI run is
# green, and only after the full gate passes here too.
dist: ci-green gate app-test
	./scripts/build-dist.sh
	./scripts/build-app.sh --dist

# The menu bar app, Debug, signed with the Developer ID (needs DDC_SIGN_IDENTITY in
# .env): build/Debug/DeepDriveCleaner.app with the release CLI in Contents/Helpers.
app:
	./scripts/build-app.sh

# The app's logic (DDCKit) runs under `swift test`, with no app host.
app-test:
	cd app/Packages/DDCKit && swift test

# The same as `make dist`, under the name story "App skeleton and build pipeline" used.
app-dist: dist

# Regenerates the AppIcon set from the status item's glyph (DriveGlyph.swift).
icons:
	mkdir -p build
	swiftc -O -o build/icons app/DeepDriveCleaner/DriveGlyph.swift scripts/icons/main.swift
	build/icons app/DeepDriveCleaner/Assets.xcassets/AppIcon.appiconset
