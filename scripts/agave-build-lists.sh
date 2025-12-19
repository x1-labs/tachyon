#!/usr/bin/env bash
# Defines reusable lists of Agave binary names for use across scripts.

# Source this file to access the arrays
# Example:
#   source "scripts/agave-build-lists.sh"
#   printf '%s\n' "${AGAVE_BINS_DEV[@]}"


# Groups with binary names to be built, based on their intended audience
# Keep names in sync with build/install scripts that consume these lists.

# shellcheck disable=SC2034
AGAVE_BINS_DEV=(
  cargo-build-sbf
  cargo-test-sbf
  x1-test-validator
)

AGAVE_BINS_END_USER=(
  agave-install
  x1
  x1-keygen
)

AGAVE_BINS_VAL_OP=(
  tachyon-validator
  tachyon-watchtower
  x1-gossip
  x1-genesis
  x1-faucet
)

AGAVE_BINS_DCOU=(
  tachyon-ledger-tool
)

# These bins are deprecated and will be removed in a future release
AGAVE_BINS_DEPRECATED=(
  solana-stake-accounts
  solana-tokens
  agave-install-init
)

DCOU_TAINTED_PACKAGES=(
  agave-ledger-tool
  agave-store-histogram
  agave-store-tool
  solana-accounts-cluster-bench
  solana-banking-bench
  solana-bench-tps
  solana-dos
  solana-local-cluster
  solana-transaction-dos
  solana-vortexor
)
