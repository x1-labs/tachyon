use {
    crate::{
        stakes::{create_and_add_stakes, StakerInfo},
        unlocks::UnlockInfo,
    },
    solana_cluster_type::ClusterType,
    solana_genesis_config::GenesisConfig,
};

#[allow(dead_code)]
fn add_stakes(
    genesis_config: &mut GenesisConfig,
    staker_infos: &[StakerInfo],
    unlock_info: &UnlockInfo,
) -> u64 {
    staker_infos
        .iter()
        .map(|staker_info| create_and_add_stakes(genesis_config, staker_info, unlock_info, None))
        .sum::<u64>()
}

/// Add accounts that should be present in genesis; skip for development clusters.
///
/// X1 does not seed Solana-foundation genesis stake accounts (CREATOR/SERVICE/
/// FOUNDATION/GRANTS/COMMUNITY); X1's genesis is configured separately, so this
/// is intentionally empty. `add_stakes` is retained (dead code) for parity.
#[allow(
    unused_variables,
    unused_assignments,
    unused_mut,
    clippy::needless_return
)]
pub fn add_genesis_stake_accounts(genesis_config: &mut GenesisConfig, mut issued_lamports: u64) {
    if genesis_config.cluster_type == ClusterType::Development {
        return;
    }
}
