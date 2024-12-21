#!/usr/bin/env bash

set -e

here="$(dirname "$0")"
src_root="$(readlink -f "${here}/..")"

cd "${src_root}"

# `cargo-audit` doesn't give us a way to do this nicely, so hammer it is...
dep_tree_filter="grep -Ev '│|└|├|─'"

while [[ -n $1 ]]; do
  if [[ $1 = "--display-dependency-trees" ]]; then
    dep_tree_filter="cat"
    shift
  fi
done

cargo_audit_ignores=(
  # Potential segfault in the time crate
  #
  # Blocked on chrono updating `time` to >= 0.2.23
  --ignore RUSTSEC-2020-0071

  # tokio: vulnerability affecting named pipes on Windows
  #
  # Exception is a stopgap to unblock CI
  # https://github.com/solana-labs/solana/issues/29586
  --ignore RUSTSEC-2023-0001

  --ignore RUSTSEC-2022-0093

  # curve25519-dalek
  --ignore RUSTSEC-2024-0344

  # Crate:     idna
  # Version:   0.1.5
  # Title:     `idna` accepts Punycode labels that do not produce any non-ASCII when decoded
  # Date:      2024-12-09
  # ID:        RUSTSEC-2024-0421
  # URL:       https://rustsec.org/advisories/RUSTSEC-2024-0421
  # Solution:  Upgrade to >=1.0.0
  # need to solve this depentant tree:
  # jsonrpc-core-client v18.0.0 -> jsonrpc-client-transports v18.0.0 -> url v1.7.2 -> idna v0.1.5
  --ignore RUSTSEC-2024-0421

  # tonic
  # When using tonic::transport::Server there is a remote DoS attack that can cause
  # the server to exit cleanly on accepting a tcp/tls stream.
  # Ignoring because we do not use this functionality.
  --ignore RUSTSEC-2024-0376

	# Crate:     idna
	# Version:   0.1.5
	# Title:     `idna` accepts Punycode labels that do not produce any non-ASCII when decoded
	# Date:      2024-12-09
	# ID:        RUSTSEC-2024-0421
	# URL:       https://rustsec.org/advisories/RUSTSEC-2024-0421
	# Solution:  Upgrade to >=1.0.0
	# need to solve this depentant tree:
	# jsonrpc-core-client v18.0.0 -> jsonrpc-client-transports v18.0.0 -> url v1.7.2 -> idna v0.1.5
	--ignore RUSTSEC-2024-0421
)
scripts/cargo-for-all-lock-files.sh audit "${cargo_audit_ignores[@]}" | $dep_tree_filter
# we want the `cargo audit` exit code, not `$dep_tree_filter`'s
exit "${PIPESTATUS[0]}"
