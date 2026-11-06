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

  # Crate:     rustls
  # Version:   0.23.36 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     TLS 1.3 handshake messages incorrectly accepted across encryption level boundaries
  # Date:      2026-09-14
  # ID:        RUSTSEC-2026-0285
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0285
  # Solution:  Upgrade to >=0.23.45
  #
  # 0.21.12 is the final 0.21.x release. 0.23.46 needs rustls-webpki >=0.103.16,
  # which conflicts with the vendored rustls-webpki in [patch.crates-io], so it is
  # not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0285

  # Crate:     quinn-proto
  # Version:   0.11.15 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Remote peer can panic quinn-proto with zero-length STREAM frames during unordered reads
  # Date:      2026-09-14
  # ID:        RUSTSEC-2026-0338
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0338
  # Solution:  Upgrade to >=0.11.18
  #
  # The patched releases raise the dependency floor (getrandom 0.4, chacha20 0.10,
  # jni 0.22), so this is not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0338

  # Crate:     quinn-proto
  # Version:   0.11.15 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Unbounded growth of pending connection ID retirements in quinn-proto
  # Date:      2026-08-17
  # ID:        RUSTSEC-2026-0339
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0339
  # Solution:  Upgrade to >=0.11.17
  #
  # The patched releases raise the dependency floor (getrandom 0.4, chacha20 0.10,
  # jni 0.22), so this is not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0339

  # Crate:     quinn-proto
  # Version:   0.11.15 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Peer minimum ACK delay can panic quinn-proto when ACK_FREQUENCY transmission is enabled
  # Date:      2026-09-14
  # ID:        RUSTSEC-2026-0340
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0340
  # Solution:  Upgrade to >=0.11.18
  #
  # The patched releases raise the dependency floor (getrandom 0.4, chacha20 0.10,
  # jni 0.22), so this is not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0340

  # Crate:     quinn-proto
  # Version:   0.11.15 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Stream reassembly memory-exhaustion guard in quinn-proto can be bypassed
  # Date:      2026-08-17
  # ID:        RUSTSEC-2026-0341
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0341
  # Solution:  Upgrade to >=0.11.17
  #
  # The patched releases raise the dependency floor (getrandom 0.4, chacha20 0.10,
  # jni 0.22), so this is not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0341

  # Crate:     quinn-proto
  # Version:   0.11.15 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Zero-length DATAGRAM frames bypass the receive buffer limit in quinn-proto
  # Date:      2026-08-17
  # ID:        RUSTSEC-2026-0342
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0342
  # Solution:  Upgrade to >=0.11.17
  #
  # The patched releases raise the dependency floor (getrandom 0.4, chacha20 0.10,
  # jni 0.22), so this is not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0342

  # Crate:     rustls
  # Version:   0.21.12 (root, dev-bins, platform-tools-sdk, programs/sbf), 0.23.36 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     `Debug` impls for client session values expose TLS secrets
  # Date:      2026-10-14
  # ID:        RUSTSEC-2026-0346
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0346
  # Solution:  Upgrade to >=0.23.46
  #
  # 0.21.12 is the final 0.21.x release. 0.23.46 needs rustls-webpki >=0.103.16,
  # which conflicts with the vendored rustls-webpki in [patch.crates-io], so it is
  # not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0346

  # Crate:     rustls
  # Version:   0.23.36 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Brotli certificate decompression can allocate up to 1 GiB before authentication
  # Date:      2026-10-14
  # ID:        RUSTSEC-2026-0348
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0348
  # Solution:  Upgrade to >=0.23.46
  #
  # 0.21.12 is the final 0.21.x release. 0.23.46 needs rustls-webpki >=0.103.16,
  # which conflicts with the vendored rustls-webpki in [patch.crates-io], so it is
  # not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0348

  # Crate:     rustls
  # Version:   0.21.12 (root, dev-bins, platform-tools-sdk, programs/sbf), 0.23.36 (root, dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Unauthenticated ServerHello can downgrade later connections to classical key exchange
  # Date:      2026-10-14
  # ID:        RUSTSEC-2026-0349
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0349
  # Solution:  Upgrade to >=0.23.46
  #
  # 0.21.12 is the final 0.21.x release. 0.23.46 needs rustls-webpki >=0.103.16,
  # which conflicts with the vendored rustls-webpki in [patch.crates-io], so it is
  # not a lockfile-only bump at this base.
  --ignore RUSTSEC-2026-0349

  # Crate:     webpki-roots
  # Version:   0.25.2 (root, programs/sbf), 0.25.4 (dev-bins, platform-tools-sdk)
  # Title:     Bundled root certificates allow trust in distrusted emSign intermediates
  # Date:      2026-10-19
  # ID:        RUSTSEC-2026-0381
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0381
  # Solution:  Upgrade to >=1.0.10
  #
  # Only the 0.25.x line remains, pulled in by reqwest 0.11, which requires ^0.25.
  # The 0.26.x and 1.x copies are on patched versions.
  --ignore RUSTSEC-2026-0381

  # Crate:     rustls-webpki
  # Version:   0.101.7 (dev-bins, platform-tools-sdk, programs/sbf)
  # Title:     Certificate verification may accept distrusted intermediates
  # Date:      2026-10-20
  # ID:        RUSTSEC-2026-0382
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0382
  # Solution:  Upgrade to >=0.103.16, <0.104.0-alpha.1 or >=0.104.0-alpha.8
  #
  # Only 0.101.7 remains. It is the final 0.101.x release and rustls 0.21.12
  # requires ^0.101.7. The 0.103.x copies are on patched versions.
  --ignore RUSTSEC-2026-0382

  # Crate:     crypto-bigint
  # Version:   0.5.5 (root, dev-bins, programs/sbf)
  # Title:     Multiple timing variability issues in operations intended to be constant-time
  # Date:      2026-10-23
  # ID:        RUSTSEC-2026-0387
  # URL:       https://rustsec.org/advisories/RUSTSEC-2026-0387
  # Solution:  Upgrade to >=0.7.6
  #
  # The fix is only in 0.7.x. elliptic-curve 0.13 (via k256) requires ^0.5.
  --ignore RUSTSEC-2026-0387
)
scripts/cargo-for-all-lock-files.sh audit "${cargo_audit_ignores[@]}" | $dep_tree_filter
# we want the `cargo audit` exit code, not `$dep_tree_filter`'s
exit "${PIPESTATUS[0]}"
