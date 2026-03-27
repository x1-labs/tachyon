# Core BPF Migration Guide for X1 Tachyon

This document describes how to migrate X1's native builtin programs to Core BPF, following the same process Solana used on mainnet.

Last updated: 2026-03-27

---

## Background

Solana migrated four native builtin programs to Core BPF (SIMD-0088). The process replaces native Rust entrypoints with on-chain BPF programs, making them upgradeable via feature-gated mechanisms.

On X1, the migration feature gates for config, ALT, and feature gate were activated but the source buffer accounts were never deployed, so the migrations silently failed. The native builtins continued to function on v2.2. However, v2.3+ and v3.0+ removed these builtins from the `BUILTINS` list, breaking ALT/config operations. Completing these migrations on v2.2 is a prerequisite for upgrading to v2.3 or v3.0.

---

## Verified Program ELFs

All ELFs below have been verified by dumping the live programs from Solana mainnet (`solana -um program dump`) and comparing SHA-256 hashes against the GitHub release artifacts.

| Program | GitHub Repo | Release Tag | ELF Size | SHA-256 |
|---------|------------|-------------|----------|---------|
| Address Lookup Table | `solana-program/address-lookup-table` | `program@v3.0.0` | 170,144 | `e264e1537c5ee1252aae1fa476c25000b641357bc6af4efab65f314160a99570` |
| Config | `solana-program/config` | `program@v3.0.0` | 157,184 | `06dd0ed33dda54b37fbbb9df5e08d28a71aa817dbc57d7b0a57e348d2f4e34fa` |
| Feature Gate | `solana-program/feature-gate` | `program@v0.0.1` | 72,984 | `4889655084aa7b8a58cb44f56a8396881a7329a1e3b91cebd9763cdf9ad04e88` |
| Stake | `solana-program/stake` | `program@v1.0.0` | 232,464 | `f35947e5e5b063b5339cd6e8a18a31b837c2edce6b8b8dd7e2762611f88f55c5` |

**Important:** Stake `v1.0.1` (200,208 bytes) does NOT match what's currently on Solana mainnet. Use `v1.0.0`.

---

## Agave Version to Program Version Mapping

This table tracks which BPF program versions correspond to each Agave release, based on when Solana activated migrations and upgrades on mainnet.

| Agave Version | ALT | Config | Feature Gate | Stake | Notes |
|---------------|-----|--------|--------------|-------|-------|
| v2.2.0 | native | native | native | native | All builtins native |
| v2.2.1 | native | v3.0.0 (epoch 753) | v0.0.1 (epoch 752) | native | Config + FG migrated Mar 2025 |
| v2.2.4 | v3.0.0 (epoch 762) | v3.0.0 | v0.0.1 | native | ALT migrated Mar 25 2025 |
| v2.2.19 | v3.0.0 | v3.0.0 | v0.0.1 | v1.0.0 (epoch 823) | Stake migrated Jul 2025 |
| v2.3.0+ | v3.0.0 | v3.0.0 | v0.0.1 | v1.0.0 | ALT+Config removed from BUILTINS |
| v3.0.0+ | v3.0.0 | v3.0.0 | v0.0.1 | v1.0.0 | All removed from BUILTINS, native code stripped |

### Pending Upgrades (not yet on Solana mainnet)

| Program | New Version | Feature Gate (Solana) | Buffer Address | Status |
|---------|-------------|----------------------|----------------|--------|
| Stake | v1.0.1 | `vote_state_v4` / `Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ` | `BM11F4hqrpinQs28sEZfzQ2fYddivYs4NEAHF6QMjkJF` | Buffer deployed, not activated |
| Stake | v5.0.0 | `upgrade_bpf_stake_program_to_v5` / `STk5Xj8hdAx3sTzmtJ3QysKkq6X2A3yj73JtxttiRyk` | `4EBQBjw1kqF1dqUBb6fc5Ji4tCEQgNf9ESGGX3smwXwh` | Not deployed |

---

## X1 Mainnet Current State

| Program | v1 Feature (already fired) | v2 Feature (new) | Buffer (new) | On-chain Owner | Migrated? |
|---------|---------------------------|-------------------|--------------|----------------|-----------|
| Stake | `8v4oWsx...` (NOT active) | *(uses v1 — not yet fired)* | `8t3vv6v...` (existing) | NativeLoader | No |
| Config | `HJQ15Ru...` (active, failed) | `3W77RxrUfZpDDfPxgRHQG6ZBi2oLq7vdTctkAoJdozjL` | `CxBudfBvfxeRb8XmDP1T2K1A6npYgfB8Pgo7wvQstCVj` | NativeLoader | No |
| ALT | `8fqszxY...` (active, failed) | `CjmzUVD3Z2jWobu5i8aeJ1MGc8gUVGeHXva5T86EaC6A` | `82M86jvpwc5s8e8NsY81pNHtspFGPC6nRv8CXhP9rs9L` | NativeLoader | No |
| Feature Gate | `7TuSw9f...` (active, failed) | `5r43RFT6ZsRJS3DpbWCJxEmDnsThJzbfuseTDKpiLrYi` | `2onpMnm4JZekhbMsWy4PTuk6kddJuvJixz41fZFDbdJM` | N/A (stateless) | No |

Config, ALT, and Feature Gate v1 features already fired but migration failed because source buffers were never deployed. New v2 feature gates have been created to retry the migration.

---

## Migration Steps

### Step 1: Release updated v2.2 validator

The v2.2 code has been updated with v2 migration feature gates and buffer addresses in `feature-set/src/lib.rs` and `builtins/src/lib.rs`. This release is **safe to deploy** — no behavior changes occur until the new features are activated. The native builtins continue to run as before.

```bash
cargo build --release
# Deploy to all validators in the cluster
```

### Step 2: Download the verified ELFs

```bash
mkdir -p ~/core-bpf-programs && cd ~/core-bpf-programs

# Address Lookup Table — v3.0.0
gh release download program@v3.0.0 \
  --repo solana-program/address-lookup-table \
  --pattern "*.so"

# Config — v3.0.0
gh release download program@v3.0.0 \
  --repo solana-program/config \
  --pattern "*.so"

# Feature Gate — v0.0.1
gh release download program@v0.0.1 \
  --repo solana-program/feature-gate \
  --pattern "*.so"

# Stake — v1.0.0 (NOT v1.0.1)
gh release download program@v1.0.0 \
  --repo solana-program/stake \
  --pattern "*.so"
```

### Step 3: Verify the downloads

```bash
echo "e264e1537c5ee1252aae1fa476c25000b641357bc6af4efab65f314160a99570  solana_address_lookup_table_program.so" | shasum -a 256 -c
echo "06dd0ed33dda54b37fbbb9df5e08d28a71aa817dbc57d7b0a57e348d2f4e34fa  solana_config_program.so" | shasum -a 256 -c
echo "4889655084aa7b8a58cb44f56a8396881a7329a1e3b91cebd9763cdf9ad04e88  solana_feature_gate_program.so" | shasum -a 256 -c
echo "f35947e5e5b063b5339cd6e8a18a31b837c2edce6b8b8dd7e2762611f88f55c5  solana_stake_program.so" | shasum -a 256 -c
```

You can also verify against Solana mainnet directly:

```bash
solana -um program dump AddressLookupTab1e1111111111111111111111111 solana_alt_ref.so
shasum -a 256 solana_alt_ref.so solana_address_lookup_table_program.so
# Hashes should match
```

### Step 4: Deploy buffer accounts

The buffer keypair files are in `~/Documents/x1-testnet/x1_feature_gate_keys/`.

```bash
solana config set --url https://rpc.mainnet.x1.xyz

# Stake — deploy to existing buffer address
solana program write-buffer solana_stake_program.so \
  --buffer ~/Documents/x1-testnet/x1_feature_gate_keys/migrate_stake_program_to_core_bpf_8v4oWsx9gG9rXREnejzNYyjpYF5oTP1igE7pqLDt9bKe.json

# Config — deploy to new v2 buffer
solana program write-buffer solana_config_program.so \
  --buffer ~/Documents/x1-testnet/x1_feature_gate_keys/config_buffer_v2_CxBudfBvfxeRb8XmDP1T2K1A6npYgfB8Pgo7wvQstCVj.json

# ALT — deploy to new v2 buffer
solana program write-buffer solana_address_lookup_table_program.so \
  --buffer ~/Documents/x1-testnet/x1_feature_gate_keys/alt_buffer_v2_82M86jvpwc5s8e8NsY81pNHtspFGPC6nRv8CXhP9rs9L.json

# Feature Gate — deploy to new v2 buffer
solana program write-buffer solana_feature_gate_program.so \
  --buffer ~/Documents/x1-testnet/x1_feature_gate_keys/feature_gate_buffer_v2_2onpMnm4JZekhbMsWy4PTuk6kddJuvJixz41fZFDbdJM.json
```

Verify the buffers were deployed:

```bash
solana account 8t3vv6v99tQA6Gp7fVdsBH66hQMaswH5qsJVqJqo8xvG  # stake buffer
solana account CxBudfBvfxeRb8XmDP1T2K1A6npYgfB8Pgo7wvQstCVj   # config buffer
solana account 82M86jvpwc5s8e8NsY81pNHtspFGPC6nRv8CXhP9rs9L   # ALT buffer
solana account 2onpMnm4JZekhbMsWy4PTuk6kddJuvJixz41fZFDbdJM   # feature gate buffer
# All should be owned by BPFLoaderUpgradeab1e
```

### Step 5: Activate feature gates

**Activate one at a time.** Wait for an epoch boundary after each activation to confirm the migration succeeds before proceeding. Monitor validator logs for:
- `"migrate_builtin_to_core_bpf"` — success
- `"Failed to migrate builtin"` — failure

```bash
# 1. Stake (uses existing feature gate — not yet active)
solana feature activate 8v4oWsx9gG9rXREnejzNYyjpYF5oTP1igE7pqLDt9bKe \
  --keypair ~/Documents/x1-testnet/x1_feature_gate_keys/migrate_stake_program_to_core_bpf_8v4oWsx9gG9rXREnejzNYyjpYF5oTP1igE7pqLDt9bKe.json

# Wait for epoch boundary, verify migration success, then:

# 2. Config (v2 feature gate)
solana feature activate 3W77RxrUfZpDDfPxgRHQG6ZBi2oLq7vdTctkAoJdozjL \
  --keypair ~/Documents/x1-testnet/x1_feature_gate_keys/migrate_config_program_to_core_bpf_v2_3W77RxrUfZpDDfPxgRHQG6ZBi2oLq7vdTctkAoJdozjL.json

# Wait for epoch boundary, verify, then:

# 3. Address Lookup Table (v2 feature gate)
solana feature activate CjmzUVD3Z2jWobu5i8aeJ1MGc8gUVGeHXva5T86EaC6A \
  --keypair ~/Documents/x1-testnet/x1_feature_gate_keys/migrate_address_lookup_table_program_to_core_bpf_v2_CjmzUVD3Z2jWobu5i8aeJ1MGc8gUVGeHXva5T86EaC6A.json

# Wait for epoch boundary, verify, then:

# 4. Feature Gate (v2 feature gate)
solana feature activate 5r43RFT6ZsRJS3DpbWCJxEmDnsThJzbfuseTDKpiLrYi \
  --keypair ~/Documents/x1-testnet/x1_feature_gate_keys/migrate_feature_gate_program_to_core_bpf_v2_5r43RFT6ZsRJS3DpbWCJxEmDnsThJzbfuseTDKpiLrYi.json
```

### Step 6: Verify migrations

After each activation and epoch boundary, confirm the program is now BPF:

```bash
solana -u https://rpc.mainnet.x1.xyz account Stake11111111111111111111111111111111111111
# Owner should be: BPFLoaderUpgradeab1e11111111111111111111111

solana -u https://rpc.mainnet.x1.xyz account Config1111111111111111111111111111111111111
solana -u https://rpc.mainnet.x1.xyz account AddressLookupTab1e1111111111111111111111111
```

Also dump and verify the on-chain ELF matches what you deployed:

```bash
solana -u https://rpc.mainnet.x1.xyz program dump Stake11111111111111111111111111111111111111 x1_stake.so
echo "f35947e5e5b063b5339cd6e8a18a31b837c2edce6b8b8dd7e2762611f88f55c5  x1_stake.so" | shasum -a 256 -c
```

---

## What Happens After Migration

Once all four migrations are complete:

- All programs run as BPF on-chain, matching Solana mainnet
- The `BUILTINS` entries with native entrypoints are skipped (runtime detects `owner == BPFLoaderUpgradeab1e`)
- The codebase is ready to upgrade to **v2.3 or v3.0** — those versions removed these builtins from the `BUILTINS` list, which is now safe because the programs are BPF
- The v3.0 fee calculation fix (checking `FeatureSet` for migration status) becomes a no-op since `migrate_stake_program_to_core_bpf` will be active

---

## Upgrade Path After Initial Migration

Once migrated, programs can be upgraded via the `upgrade_core_bpf_program` mechanism:

1. Build/obtain the new ELF version
2. Deploy to a new buffer account on X1 mainnet
3. Add a new feature gate to `feature-set/src/lib.rs`
4. Add the upgrade config to the runtime (similar to migration config)
5. Deploy updated validator
6. Activate the upgrade feature gate
7. At next epoch boundary, the runtime swaps the ELF

All migrated programs have `upgrade_authority: None`, so they can ONLY be upgraded through this feature-gate mechanism, not through normal `solana program upgrade`.

---

## Verification: Dumping On-Chain Programs

To verify programs match expected ELFs at any time:

```bash
# Dump from Solana mainnet (reference)
solana -um program dump AddressLookupTab1e1111111111111111111111111 solana_alt.so
solana -um program dump Config1111111111111111111111111111111111111 solana_config.so
solana -um program dump Stake11111111111111111111111111111111111111 solana_stake.so
solana -um program dump Feature111111111111111111111111111111111111 solana_feature_gate.so

# Dump from X1 mainnet (after migration)
solana -u https://rpc.mainnet.x1.xyz program dump AddressLookupTab1e1111111111111111111111111 x1_alt.so
solana -u https://rpc.mainnet.x1.xyz program dump Config1111111111111111111111111111111111111 x1_config.so
solana -u https://rpc.mainnet.x1.xyz program dump Stake11111111111111111111111111111111111111 x1_stake.so

# Compare
shasum -a 256 solana_alt.so x1_alt.so
shasum -a 256 solana_config.so x1_config.so
shasum -a 256 solana_stake.so x1_stake.so
```

---

## Keypair Inventory

All keypairs are stored in `~/Documents/x1-testnet/x1_feature_gate_keys/`.

### Feature Gate Keypairs

| Purpose | File | Pubkey |
|---------|------|--------|
| Stake migration (v1) | `migrate_stake_program_to_core_bpf_8v4oWsx9gG9rXREnejzNYyjpYF5oTP1igE7pqLDt9bKe.json` | `8v4oWsx9gG9rXREnejzNYyjpYF5oTP1igE7pqLDt9bKe` |
| Config migration (v2) | `migrate_config_program_to_core_bpf_v2_3W77RxrUfZpDDfPxgRHQG6ZBi2oLq7vdTctkAoJdozjL.json` | `3W77RxrUfZpDDfPxgRHQG6ZBi2oLq7vdTctkAoJdozjL` |
| ALT migration (v2) | `migrate_address_lookup_table_program_to_core_bpf_v2_CjmzUVD3Z2jWobu5i8aeJ1MGc8gUVGeHXva5T86EaC6A.json` | `CjmzUVD3Z2jWobu5i8aeJ1MGc8gUVGeHXva5T86EaC6A` |
| Feature Gate migration (v2) | `migrate_feature_gate_program_to_core_bpf_v2_5r43RFT6ZsRJS3DpbWCJxEmDnsThJzbfuseTDKpiLrYi.json` | `5r43RFT6ZsRJS3DpbWCJxEmDnsThJzbfuseTDKpiLrYi` |

### Buffer Keypairs

| Purpose | File | Pubkey |
|---------|------|--------|
| Stake buffer (v1) | *(existing — see keypair inventory)* | `8t3vv6v99tQA6Gp7fVdsBH66hQMaswH5qsJVqJqo8xvG` |
| Config buffer (v2) | `config_buffer_v2_CxBudfBvfxeRb8XmDP1T2K1A6npYgfB8Pgo7wvQstCVj.json` | `CxBudfBvfxeRb8XmDP1T2K1A6npYgfB8Pgo7wvQstCVj` |
| ALT buffer (v2) | `alt_buffer_v2_82M86jvpwc5s8e8NsY81pNHtspFGPC6nRv8CXhP9rs9L.json` | `82M86jvpwc5s8e8NsY81pNHtspFGPC6nRv8CXhP9rs9L` |
| Feature Gate buffer (v2) | `feature_gate_buffer_v2_2onpMnm4JZekhbMsWy4PTuk6kddJuvJixz41fZFDbdJM.json` | `2onpMnm4JZekhbMsWy4PTuk6kddJuvJixz41fZFDbdJM` |

---

## References

- [SIMD-0088: Enable Core BPF Programs](https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0088-enable-core-bpf-programs.md)
- [SIMD-0128: Migrate ALT to Core BPF](https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0128-migrate-address-lookup-table-to-core-bpf.md)
- [SIMD-0140: Migrate Config to Core BPF](https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0140-migrate-config-to-core-bpf.md)
- [Agave fetch-core-bpf.sh](https://github.com/anza-xyz/agave/blob/master/fetch-core-bpf.sh)
- [Core BPF Migration Project Board](https://github.com/orgs/solana-program/projects/5)
- Migration runtime code: `runtime/src/bank/builtins/core_bpf_migration/mod.rs`
- Builtin configs: `builtins/src/lib.rs`
