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
  # Crate:     ed25519-dalek
  # Version:   1.0.1
  # Title:     Double Public Key Signing Function Oracle Attack on `ed25519-dalek`
  # Date:      2022-06-11
  # ID:        RUSTSEC-2022-0093
  # URL:       https://rustsec.org/advisories/RUSTSEC-2022-0093
  # Solution:  Upgrade to >=2
  --ignore RUSTSEC-2022-0093

  # Crate:     idna
  # Version:   0.1.5
  # Title:     `idna` accepts Punycode labels that do not produce any non-ASCII when decoded
  # Date:      2024-12-09
  # ID:        RUSTSEC-2024-0421
  # URL:       https://rustsec.org/advisories/RUSTSEC-2024-0421
  # Solution:  Upgrade to >=1.0.0
  # need to solve this dependant tree:
  # jsonrpc-core-client v18.0.0 -> jsonrpc-client-transports v18.0.0 -> url v1.7.2 -> idna v0.1.5
  --ignore RUSTSEC-2024-0421

  # Crate:     curve25519-dalek
  # Version:   3.2.1
  # Title:     Timing variability in `curve25519-dalek`'s `Scalar29::sub`/`Scalar52::sub`
  # Date:      2024-06-18
  # ID:        RUSTSEC-2024-0344
  # URL:       https://rustsec.org/advisories/RUSTSEC-2024-0344
  # Solution:  Upgrade to >=4.1.3
  --ignore RUSTSEC-2024-0344

  # Crate:     tonic
  # Version:   0.9.2
  # Title:     Remotely exploitable Denial of Service in Tonic
  # Date:      2024-10-01
  # ID:        RUSTSEC-2024-0376
  # URL:       https://rustsec.org/advisories/RUSTSEC-2024-0376
  # Solution:  Upgrade to >=0.12.3
  --ignore RUSTSEC-2024-0376


  # Crate:     rustls-webpki
  # Version:   0.101.7
  # Title:     Name constraints for URI names were incorrectly accepted
  # Date:      2026-04-14
  # ID:        RUSTSEC-2026-0098
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0098
  # Solution:  Upgrade to >=0.103.12, <0.104.0-alpha.1 OR >=0.104.0-alpha.6
  #
  # SCOPE (verified 2026-09-02): the root workspace resolves rustls-webpki
  # through the [patch.crates-io] anza-xyz vendored tags (anza-0.101.7-2 and
  # anza-0.103.10-2), which carry the upstream fixes. That patch does NOT reach
  # dev-bins, platform-tools-sdk or programs/sbf, which resolve from the
  # registry. Their 0.103.x line was bumped to 0.103.13 (clearing
  # RUSTSEC-2026-0049 outright, ignore since removed), but they still carry
  # registry rustls-webpki 0.101.7, which is the terminal 0.101 release with no
  # backport, pulled by rustls 0.21.12 (requires ^0.101.7). These are build and
  # test tooling workspaces, not the validator. Re-check when rustls 0.21.x is
  # dropped from those trees.
  --ignore RUSTSEC-2026-0098

  # Crate:     rustls-webpki
  # Version:   0.101.7
  # Title:     Name constraints were accepted for certificates asserting a wildcard name
  # Date:      2026-04-14
  # ID:        RUSTSEC-2026-0099
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0099
  # Solution:  Upgrade to >=0.103.12, <0.104.0-alpha.1 OR >=0.104.0-alpha.6
  #
  # Same scope as RUSTSEC-2026-0098 above.
  --ignore RUSTSEC-2026-0099

  # Crate:     rustls-webpki
  # Version:   0.101.7
  # Title:     Reachable panic in certificate revocation list parsing
  # Date:      2026-04-22
  # ID:        RUSTSEC-2026-0104
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0104
  # Solution:  Upgrade to >=0.103.13, <0.104.0-alpha.1 OR >=0.104.0-alpha.7
  #
  # Same scope as RUSTSEC-2026-0098 above.
  --ignore RUSTSEC-2026-0104

  # Crate:     h2
  # Version:   0.3.26 (root, programs/sbf), 0.3.27 (dev-bins, platform-tools-sdk)
  # Title:     h2 unbounded empty DATA frames
  # Date:      2026-08-17
  # ID:        RUSTSEC-2026-0258
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0258
  # Severity:  low
  # Solution:  Upgrade to >=0.4.16
  #
  # NO FIX AT THIS BASE (accepted risk, X1): the only patched range is >=0.4.16.
  # h2 0.3.27 is the final 0.3.x release and predates the advisory, so no 0.3.x
  # backport exists. Three parents pull h2 0.3 and all hard-require ^0.3:
  # tonic 0.9.2 (via solana-storage-bigtable), hyper 0.14.32, and reqwest 0.11.27
  # (non-optional). `cargo update -p h2 --precise 0.4.16` therefore fails in every
  # affected workspace. Both reachable paths are OUTBOUND clients to known hosts:
  # gRPC to Google BigTable, and the toolchain download in platform-tools-sdk,
  # which has no tonic at all. The validator exposes no h2 listener, and the flaw
  # is a server queueing attacker-supplied empty DATA frames, so triggering it
  # needs a hostile endpoint. Clearing it requires agave PR #11093 (dc5d96f907):
  # tonic 0.9.2 -> 0.14.x with prost 0.11 -> 0.14, http 0.2 -> 1.1, hyper 0.14 ->
  # hyper-util and regenerated bigtable protos. That never landed on v4.0.
  #
  # WARNING for the next rebase: v4.1 and v4.2 pin h2 0.4.13, which is STILL
  # below 0.4.16, so audit keeps firing there and this ignore is NOT removable on
  # arrival. Only agave master carries the fix (ab0821e5d7, PR #14694). Post-
  # rebase it does become lockfile-only, because h2's only parents are then
  # hyper 1.x and tonic 0.14.x, both on ^0.4:
  #   scripts/cargo-for-all-lock-files.sh update -p h2 --precise 0.4.16
  # Drop this ignore at that point, not merely because the base moved.
  --ignore RUSTSEC-2026-0258
)
scripts/cargo-for-all-lock-files.sh audit "${cargo_audit_ignores[@]}" | $dep_tree_filter
# we want the `cargo audit` exit code, not `$dep_tree_filter`'s
exit "${PIPESTATUS[0]}"
