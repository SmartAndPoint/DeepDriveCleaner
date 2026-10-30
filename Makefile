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

# Cases per property test (proptest reads PROPTEST_CASES), set here only: 256 in
# `gate`, which CI runs and whose nextest inherits it, 32 in `gate-fast` on its own,
# the pre-commit tier whose time is already spent. A value from the environment or
# the command line (the nightly's) wins.
ifeq ($(origin PROPTEST_CASES),undefined)
PROPTEST_CASES = 32
gate: PROPTEST_CASES = 256
endif
export PROPTEST_CASES
# The declared minimum Rust version, read from Cargo.toml so it is set in one place.
MSRV := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)

.PHONY: build fmt fmt-check clippy test check release install tag require-tag dist app app-test app-dist icons \
	gate-fast gate nextest doctest clippy-dhat doc deny machete fuzz-lock msrv release-check unported-check \
	tools require-tools hooks coverage coverage-ratchet ci-green golden-check golden-record loadgen-clean load-image load load-baseline scaling heap-profile bench-test script-test leaks asan fuzz ddckit-test ci-green-full

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
# (.githooks/pre-commit), gate before every push (.githooks/pre-push) and in CI; the
# hooks and CI skip them when only Rust-free paths changed (scripts/gate-scope.sh,
# .config/rust-free-paths; story "Gate by what changed: load at night, one gate per
# release, app-only pushes skip Rust", DDC-005).
gate-fast: require-tools fmt-check clippy nextest doctest

# deny, machete and fuzz-lock read only metadata and take seconds: they go first.
gate: require-tools deny machete fuzz-lock gate-fast clippy-dhat doc msrv release-check unported-check bench-test script-test leaks

# The install script's tests join the gate on a line of their own.
gate: install-test

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

# The release CLI never carries the cost counters (ddc-core feature op-stats): their
# marker symbol is absent.
release-check:
	$(CARGO) build --workspace --release --locked
	@nm target/release/deepdrivecleaner > target/release/deepdrivecleaner.symbols
	@grep ' _main$$' target/release/deepdrivecleaner.symbols >/dev/null || { \
	  echo "release-check: nm sees no symbol table, so it cannot see the counters" >&2; exit 1; }
	@if grep DDC_OP_STATS target/release/deepdrivecleaner.symbols >/dev/null; then \
	  echo "release-check: the release CLI carries the op-stats counters" >&2; exit 1; fi

# A build that is not supported (ADR "Platform seam: one platform module per OS with
# declared capabilities", section 8): ddc-core and the CLI with simulate-unported, so
# SUPPORTED is false on this Mac. It builds into its own target folder, so the
# feature never reaches target/debug or target/release, and runs only the two test
# files compiled for it; everything else assumes a supported platform.
UNPORTED := CARGO_TARGET_DIR=target/unported
unported-check:
	$(UNPORTED) $(CARGO) nextest run -p ddc-core -p ddc-cli --features ddc-cli/simulate-unported \
	  --test unported --test unported_cli --locked
	$(UNPORTED) $(CARGO) clippy -p ddc-core -p ddc-cli --all-targets --features ddc-cli/simulate-unported \
	  --locked -- -D warnings

# Golden outputs of the S load trees (bench/golden/, story "Synthetic load trees:
# large, wide, deep and hostile"). The check is part of `nextest`, so the gate runs
# it; golden-record accepts a deliberate change. Review the diff before committing it.
golden-check:
	$(CARGO) nextest run -p ddc-cli --test golden_cli --locked

golden-record:
	DDC_UPDATE_GOLDEN=1 $(CARGO) nextest run -p ddc-cli --test golden_cli --locked

# Load trees past S on a disposable disk image, each scanned and checked against its
# manifest: `make load-image LOAD_PROFILE=mixed LOAD_SIZE=L` (LOAD_PROFILE=all for every
# profile; LOAD_SIZE is S, M, L or XL). The nightly run checks every profile at M and
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
# all here. LOAD_LIMITS=enforce fails over bench/load-limits.tsv (CPU only with 3 runs
# or more, from a start load average of at most a quarter of the CPUs); record judges
# nothing. A full enforce run of seed 1 with CPU judged lands in
# bench/load-<date>-<size>.tsv, never over an existing one, and is compared with the
# newest earlier table of its size.
LOAD_REPEAT ?= 3
LOAD_LIMITS ?= enforce
LOAD_DATE := $(shell date -u +%F)
LOAD_RUN = $(if $(filter command line environment,$(origin LOAD_PROFILE)),$(LOAD_PROFILE),all)
load:
	$(CARGO) build --release -p ddc-loadgen -p ddc-cli --locked
	LOADGEN=target/release/ddc-loadgen LOAD_CLI=target/release/deepdrivecleaner LOAD_REPEAT=$(LOAD_REPEAT) \
	  LOAD_MODE=$(LOAD_LIMITS) LOAD_DATE=$(LOAD_DATE) LOAD_COMMIT=$$(git describe --always --dirty) \
	  scripts/load-image.sh $(LOAD_RUN) $(LOAD_SIZE) $(LOAD_SEED)
	@if [ "$(LOAD_RUN)" = all ] && [ "$(LOAD_SEED)" = 1 ] && head -1 target/load/load-$(LOAD_SIZE).tsv | grep -q ' mode=enforce '; then \
	  target/release/ddc-loadgen table target/load/load-$(LOAD_SIZE).tsv \
	    --into bench/load-$(LOAD_DATE)-$(LOAD_SIZE).tsv && \
	  python3 -I bench/compare_load.py bench/load-$(LOAD_DATE)-$(LOAD_SIZE).tsv; \
	else \
	  echo "make load: the table stays in target/load/load-$(LOAD_SIZE).tsv; only a full enforce run of seed 1 with CPU judged goes to bench/"; \
	fi

# The limits from S and M runs on the reference machine, with 25% headroom
# (ops/profiling-the-core.md): base from S, the slope between S and M.
load-baseline:
	$(MAKE) load LOAD_PROFILE=all LOAD_SIZE=S LOAD_SEED=1 LOAD_REPEAT=3 LOAD_LIMITS=record
	$(MAKE) load LOAD_PROFILE=all LOAD_SIZE=M LOAD_SEED=1 LOAD_REPEAT=3 LOAD_LIMITS=record
	target/release/ddc-loadgen limits target/load/load-S.tsv target/load/load-M.tsv >target/load/load-limits.tsv
	cp target/load/load-limits.tsv bench/load-limits.tsv

# Scaling slopes (story "Complexity guard: operation counts and scaling exponents"):
# the release CLI on SCALING_PROFILE (all, or names joined by commas) at an eighth, a
# quarter, half and all of SCALING_SIZE, 3 runs each, then `ddc-loadgen slopes`: the
# log-log slope per command of CPU time and of peak memory above its base, judged between
# the two largest sizes, and the fit of user time through all four sizes
# (ops/profiling-the-core.md). SCALING_LIMITS=enforce
# fails over 1.2. A run with CPU judged (a quiet start, read before the build), seed 1,
# of every profile or, at L and XL, of SCALING_L_PROFILES lands in
# bench/scaling-<date>-<size>.tsv, never over an existing one.
SCALING_PROFILE ?= all
SCALING_SIZE ?= M
SCALING_LIMITS ?= enforce
SCALING_L_PROFILES ?= mixed,wide-root,projects,users
scaling:
	@case "$(SCALING_SIZE)" in M | L | XL) ;; \
	  *) echo "make scaling: SCALING_SIZE is M, L or XL, not $(SCALING_SIZE)" >&2; exit 2 ;; esac
	@mkdir -p target/scaling && sysctl -n vm.loadavg | awk '{ print $$2 }' > target/scaling/load-start
	$(CARGO) build --release -p ddc-loadgen -p ddc-cli --locked
	rm -rf target/scaling/$(SCALING_SIZE)
	@case "$(SCALING_SIZE)" in M) tier=100000 ;; L) tier=1000000 ;; XL) tier=10000000 ;; esac; \
	start=$$(cat target/scaling/load-start); began=$$(date +%s); \
	for d in 8 4 2 1; do \
	  n=$$((tier / d)); \
	  LOADGEN=target/release/ddc-loadgen LOAD_CLI=target/release/deepdrivecleaner LOAD_REPEAT=3 LOAD_MODE=record \
	    LOAD_DATE=$(LOAD_DATE) LOAD_COMMIT=$$(git describe --always --dirty) LOAD_ENTRIES=$$n LOAD_START=$$start \
	    LOAD_OUT=target/scaling/$(SCALING_SIZE)/$$n scripts/load-image.sh $(SCALING_PROFILE) $(SCALING_SIZE) $(LOAD_SEED) || exit 1; \
	done; \
	echo "make scaling: the four sizes took $$(( ($$(date +%s) - began) / 60 )) min from a 1-minute load of $$start"
	target/release/ddc-loadgen slopes target/scaling/$(SCALING_SIZE)/*/load-$(SCALING_SIZE).tsv \
	  --mode $(SCALING_LIMITS) --into target/scaling/$(SCALING_SIZE)/scaling.tsv; \
	  status=$$?; cat target/scaling/$(SCALING_SIZE)/scaling.tsv; exit $$status
	@if { [ "$(SCALING_PROFILE)" = all ] || { [ "$(SCALING_SIZE)" != M ] && [ "$(SCALING_PROFILE)" = "$(SCALING_L_PROFILES)" ]; }; } \
	  && [ "$(LOAD_SEED)" = 1 ] && head -1 target/scaling/$(SCALING_SIZE)/scaling.tsv | grep -q ' mode=enforce '; then \
	  out=bench/scaling-$(LOAD_DATE)-$(SCALING_SIZE).tsv; \
	  if [ -e $$out ]; then echo "make scaling: $$out exists and is never replaced" >&2; exit 1; fi; \
	  cp target/scaling/$(SCALING_SIZE)/scaling.tsv $$out && echo "slopes: $$out"; \
	else \
	  echo "make scaling: the slopes stay in target/scaling/$(SCALING_SIZE)/scaling.tsv; only a run with CPU judged, seed 1, of every profile or, at L and XL, of SCALING_L_PROFILES goes to bench/"; \
	fi

# The allocation sites of one scan (dhat): HEAP_PROFILE at HEAP_SIZE on a disk image,
# by a CLI built with ddc-core/dhat-heap in the `profiling` profile into target/dhat,
# so it never replaces the release binary. bench/dhat_top.py prints the top 10 program
# points by bytes live at the global peak and by total bytes, per entry.
HEAP_PROFILE ?= mixed
HEAP_SIZE ?= M
heap-profile:
	@[ "$(HEAP_PROFILE)" != all ] || { echo "make heap-profile: one profile at a time, not all" >&2; exit 2; }
	CARGO_TARGET_DIR=target/dhat $(CARGO) build --profile profiling -p ddc-cli --features ddc-core/dhat-heap --locked
	$(CARGO) build --release -p ddc-loadgen --locked
	LOADGEN=target/release/ddc-loadgen LOAD_HEAP=target/dhat/profiling/deepdrivecleaner LOAD_OUT=target/dhat \
	  scripts/load-image.sh $(HEAP_PROFILE) $(HEAP_SIZE) 1
	python3 -I bench/dhat_top.py target/dhat/dhat-$(HEAP_SIZE)-$(HEAP_PROFILE).json \
	  --entries $$(plutil -extract entries raw -o - target/dhat/$(HEAP_SIZE)-$(HEAP_PROFILE).json)

# The tests of the bench scripts.
bench-test:
	python3 -I -m unittest discover -s bench -p 'test_*.py'

# The tests of the release scripts (scripts/tests/): the appcast render with a fake
# sign_update, no network; the signing check with a fake codesign. Its ad-hoc fixture
# signs, so it runs in app-test only.
script-test:
	python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'

# packaging/install/install.sh, the one-line install (story "One-line install on macOS
# and Linux: ddc.sh/install.sh and cargo", DDC-007): install-test runs it against fake
# releases on 127.0.0.1 (part of `make gate`); install-lint checks it as POSIX sh (CI's
# Install workflow).
.PHONY: install-test install-lint
install-test:
	python3 -I -m unittest discover -s packaging/install -p 'test_*.py'

install-lint:
	@command -v shellcheck >/dev/null || { echo "install-lint: shellcheck is missing: brew install shellcheck" >&2; exit 1; }
	shellcheck -s sh packaging/install/install.sh

# `scan --json` of every profile's S tree under `leaks -atExit`, on the cargo release
# build (the notarized app's hardened runtime keeps `leaks` out): fails on any leak.
# Outputs in target/load/leaks-<profile>.txt and .stderr.txt. Part of `make gate`
# since the owner's first run on the reference machine (2026-10-29, 0 leaks).
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

# libFuzzer on the parsers of outside input, FUZZ_SECONDS per target, on the nightly
# of `asan` (story "Coverage floor and property tests for the safety invariants",
# DDC-005). fuzz/ is a workspace of its own; cargo fuzz takes no --locked, so its lock
# is checked first. fuzz/regressions seeds the corpus (fuzz/corpus, not committed); a
# crash found is kept there, minimized, and ddc-core/tests/fuzz_regressions.rs runs it.
# Every target runs even after one crashes; the nightly workflow runs it longer.
FUZZ_SECONDS ?= 60
FUZZ_TARGETS := disk_plist listed_items action_list settings last_scan
fuzz: fuzz-lock
	$(call install-tool,fuzz,$(FUZZ_VERSION))
	rustup toolchain install $(ASAN_TOOLCHAIN) --profile minimal
	@status=0; for t in $(FUZZ_TARGETS); do \
		mkdir -p fuzz/corpus/$$t; \
		$(CARGO) +$(ASAN_TOOLCHAIN) fuzz run $$t fuzz/corpus/$$t fuzz/regressions/$$t -- \
		  -max_total_time=$(FUZZ_SECONDS) -timeout=10 || status=1; \
	done; exit $$status

# fuzz/Cargo.lock records the workspace version of ddc-core, so every version bump makes
# it stale; the gate checks it on the stable toolchain, and the nightly fuzz run needs it.
fuzz-lock:
	@$(CARGO) metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null || { \
		echo "fuzz/Cargo.lock is stale: run cargo update -p ddc-core --manifest-path fuzz/Cargo.toml and commit it" >&2; \
		exit 1; }

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

# Line and region coverage under target/coverage, so the tree stays clean for
# build-dist.sh: lcov.info, summary.json and html/. Fails when a crate or a safety
# file drops under bench/coverage-floor.tsv (story "Coverage floor and property
# tests for the safety invariants", DDC-005); CI's coverage job and the nightly run it.
coverage:
	mkdir -p target/coverage
	$(CARGO) llvm-cov nextest --workspace $(FEATURES) --locked --lcov --output-path target/coverage/lcov.info
	$(CARGO) llvm-cov report --json --summary-only --output-path target/coverage/summary.json
	$(CARGO) llvm-cov report --html --output-dir target/coverage
	$(CARGO) llvm-cov report --summary-only
	python3 -I bench/coverage_floor.py check target/coverage/summary.json bench/coverage-floor.tsv

# The owner, on main: raises bench/coverage-floor.tsv from the summary of the latest
# green Gate run on main whose coverage job ran (an app-only push skips it), never lowers
# it. CI never runs this. Commit the result.
coverage-ratchet:
	rm -rf target/coverage/ci
	@run=""; for id in $$(gh run list --workflow gate.yml --branch main --event push --status success --limit 20 \
		--json databaseId --jq '.[].databaseId'); do \
		[ "$$(gh api "repos/{owner}/{repo}/actions/runs/$$id/jobs?filter=latest&per_page=100" \
		  --jq '[.jobs[] | select(.name == "coverage" and .conclusion == "success")] | length')" = 1 ] && { run=$$id; break; }; \
	done; [ -n "$$run" ] || { echo "no green Gate run on main with a coverage job in the last 20" >&2; exit 1; }; \
	gh run download "$$run" --name coverage --dir target/coverage/ci
	python3 -I bench/coverage_floor.py ratchet target/coverage/ci/summary.json bench/coverage-floor.tsv

# Refuses unless the Gate workflow passed on GitHub for HEAD (scripts/ci-green.sh
# release.yml asks the same of the release workflow). ci-green-full also requires HEAD's
# own run to have run and passed its Rust jobs: what a release rests on.
ci-green:
	./scripts/ci-green.sh

ci-green-full:
	./scripts/ci-green.sh --full

release:
	$(CARGO) build --workspace --release

# Installs `deepdrivecleaner` into ~/.cargo/bin and `ddc` as a symlink to it
# (one binary, so macOS privacy grants such as Full Disk Access apply to both).
install:
	$(CARGO) install --path crates/ddc-cli --locked
	ln -sf "$(HOME)/.cargo/bin/deepdrivecleaner" "$(HOME)/.cargo/bin/ddc"

# A release, in this order (story "Build matrix: every target built, tested and
# packaged from one tag", DDC-007):
#   1. make tag: the owner pushes the annotated tag v<version> at HEAD of origin/main,
#      after a green Gate run for it. Nothing else creates the tag.
#   2. release.yml runs on the tag: the enabled targets' archives, as workflow artifacts.
#   3. make dist: the macOS assets from the tagged commit.
#   4. scripts/publish-release.sh: one release with all of them and SHA256SUMS.
# The release is one commit: nothing is pushed to main between make tag and publish.
# If release.yml fails, or a fix needs a commit, first check that the tag has no
# release ('gh release view v<version>' says release not found), then fix on main,
# delete the tag on origin (git push origin :refs/tags/v<version>) and locally, and
# run make tag again; or bump the version. publish-release.sh refuses a red run only
# while RELEASE_TARGETS is non-empty. Never delete the tag of a published release.
tag:
	./scripts/tag-release.sh

# Origin holds v<version> at HEAD: the first step of make dist, before ci-green.
require-tag:
	./scripts/require-tag.sh

# Both macOS release assets in dist/, from one commit (needs .env with DDC_SIGN_IDENTITY
# and NOTARY_PROFILE): the signed, notarized CLI tarball, then the notarized, stapled
# app zip. Only from the commit `make tag` tagged on origin, whose own CI run is green with
# its Rust jobs: that run is `make gate` and `make app-test` on this very commit, so they
# do not run here again.
dist: require-tag ci-green-full
	./scripts/build-dist.sh
	./scripts/build-app.sh --dist

# The menu bar app, Debug, signed with the Developer ID (needs DDC_SIGN_IDENTITY in
# .env): build/Debug/DeepDriveCleaner.app with the release CLI in Contents/Helpers.
app:
	./scripts/build-app.sh

# The app's logic (DDCKit) runs under `swift test`, with no app host (ddckit-test, which
# the pre-push hook runs alone when only DDCKit and other Rust-free paths changed). Then
# the per-binary signing check on a fixture signed ad hoc (`codesign -s -`): it must fail.
app-test: ddckit-test
	DDC_RUN_CODESIGN_TESTS=1 python3 -I -m unittest discover -s scripts/tests -p 'test_signing_check.py'

ddckit-test:
	cd app/Packages/DDCKit && swift test

# The same as `make dist`, under the name story "App skeleton and build pipeline" used.
app-dist: dist

# Regenerates the AppIcon set from the status item's glyph (DriveGlyph.swift).
icons:
	mkdir -p build
	swiftc -O -o build/icons app/DeepDriveCleaner/DriveGlyph.swift scripts/icons/main.swift
	build/icons app/DeepDriveCleaner/Assets.xcassets/AppIcon.appiconset
