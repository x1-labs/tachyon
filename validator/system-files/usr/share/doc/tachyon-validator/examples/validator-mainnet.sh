#!/bin/bash
export RUST_LOG=solana_metrics=warn,info

exec tachyon-validator \
  --identity "$HOME/.config/solana/identity.json" \
  --vote-account "$HOME/.config/solana/vote.json" \
  --entrypoint entrypoint0.mainnet.x1.xyz:8001 \
  --entrypoint entrypoint1.mainnet.x1.xyz:8001 \
  --entrypoint entrypoint2.mainnet.x1.xyz:8001 \
  --entrypoint entrypoint3.mainnet.x1.xyz:8001 \
  --entrypoint entrypoint4.mainnet.x1.xyz:8001 \
  --known-validator 7ufaUVtQKzGu5tpFtii9Cg8kR4jcpjQSXwsF3oVPSMZA \
  --known-validator 5Rzytnub9yGTFHqSmauFLsAbdXFbehMwPBLiuEgKajUN \
  --known-validator 4V2QkkWce8bwTzvvwPiNRNQ4W433ZsGQi9aWU12Q8uBF \
  --known-validator CkMwg4TM6jaSC5rJALQjvLc51XFY5pJ1H9f1Tmu5Qdxs \
  --known-validator 7J5wJaH55ZYjCCmCMt7Gb3QL6FGFmjz5U8b6NcbzfoTy \
  --only-known-rpc \
  --log - \
  --ledger "$HOME/ledger" \
  --rpc-port 8899 \
  --dynamic-port-range 8000-8030 \
  --wal-recovery-mode skip_any_corrupted_record \
