use {
    crate::{
        stakes::{create_and_add_stakes, StakerInfo},
        unlocks::UnlockInfo,
    },
    solana_sdk::genesis_config::{ClusterType, GenesisConfig},
};

// no lockups
const UNLOCKS_ALL_DAY_ZERO: UnlockInfo = UnlockInfo {
    cliff_fraction: 1.0,
    cliff_years: 0.0,
    unlocks: 0,
    unlock_years: 0.0,
    custodian: "Mc5XB47H3DKJHym5RLa9mPzWv5snERsF3KNv5AauXK8",
};

pub const CREATOR_STAKER_INFOS: &[StakerInfo] = &[];

pub const SERVICE_STAKER_INFOS: &[StakerInfo] = &[];

pub const FOUNDATION_STAKER_INFOS: &[StakerInfo] = &[];

pub const GRANTS_STAKER_INFOS: &[StakerInfo] = &[];

pub const COMMUNITY_STAKER_INFOS: &[StakerInfo] = &[];

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

/// Add acounts that should be present in genesis; skip for development clusters
#[allow(unused_variables, unused_assignments)]
pub fn add_genesis_accounts(genesis_config: &mut GenesisConfig, mut issued_lamports: u64) {
    if genesis_config.cluster_type == ClusterType::Development {
        return;
    }

    // add_stakes() and add_validators() award tokens for rent exemption and
    //  to cover an initial transfer-free period of the network
    issued_lamports += add_stakes(
        genesis_config,
        FOUNDATION_STAKER_INFOS,
        &UNLOCKS_ALL_DAY_ZERO,
    ) + add_stakes(genesis_config, GRANTS_STAKER_INFOS, &UNLOCKS_ALL_DAY_ZERO)
        + add_stakes(
            genesis_config,
            COMMUNITY_STAKER_INFOS,
            &UNLOCKS_ALL_DAY_ZERO,
        );
}

#[cfg(test)]
mod tests {
    use {super::*, solana_sdk::native_token::LAMPORTS_PER_SOL};

    #[test]
    fn test_add_genesis_accounts() {
        let mut genesis_config = GenesisConfig::default();

        add_genesis_accounts(&mut genesis_config, 0);

        let lamports = genesis_config
            .accounts
            .values()
            .map(|account| account.lamports)
            .sum::<u64>();

        assert_eq!(500_000_000 * LAMPORTS_PER_SOL, lamports);
    }
}
