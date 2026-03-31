#!/bin/bash
export RUST_LOG=solana_metrics=warn,info

exec tachyon-validator \
  --identity "$HOME/.config/solana/identity.json" \
  --no-voting \
  --entrypoint entrypoint0.testnet.x1.xyz:8000 \
  --entrypoint entrypoint1.testnet.x1.xyz:8001 \
  --entrypoint entrypoint2.testnet.x1.xyz:8000 \
  --entrypoint entrypoint3.testnet.x1.xyz:8000 \
  --known-validator 4qPAYtvDwq8nAY2swMrKHbZi7Rdpp3A9GMJwZ8nPtiEY \
  --known-validator Abt4r6uhFs7yPwR3jT5qbnLjBtasgHkRVAd1W6H5yonT \
  --known-validator FcrZRBfVk2h634L9yvkysJdmvdAprq1NM4u263NuR6LC \
  --known-validator Tpsu5EYTJAXAat19VEh54zuauHvUBuryivSFRC3RiFk \
  --only-known-rpc \
  --log - \
  --ledger "$HOME/ledger" \
  --rpc-port 8899 \
  --full-rpc-api \
  --dynamic-port-range 8000-8030 \
  --wal-recovery-mode skip_any_corrupted_record \
  --enable-rpc-transaction-history \
  --enable-extended-tx-metadata-storage \
  --rpc-pubsub-enable-block-subscription \
