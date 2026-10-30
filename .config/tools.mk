# Pinned versions of the gate's cargo tools: the one place they are set. `make tools`
# installs them; CI keys its tool cache on this file.
NEXTEST_VERSION := 0.9.150
DENY_VERSION := 0.20.2
MACHETE_VERSION := 0.9.2
LLVM_COV_VERSION := 0.9.1
# `make fuzz` only, outside the gate.
FUZZ_VERSION := 0.13.2
