//! stakes generator
use {
    crate::{
        address_generator::AddressGenerator,
        unlocks::{UnlockInfo, Unlocks},
    },
    solana_sdk::{
        account::Account,
        clock::Slot,
        genesis_config::GenesisConfig,
        pubkey::Pubkey,
        stake::{
            self,
            state::{Authorized, Lockup, StakeStateV2},
        },
        system_program,
        timing::years_as_slots,
    },
    solana_stake_program::stake_state::create_lockup_stake_account,
};

#[derive(Debug)]
pub struct StakerInfo {
    pub name: &'static str,
    pub staker: &'static str,
    pub withdrawer: Option<&'static str>,
    pub lamports: u64,
}

// lamports required to run staking operations for one year
//  the staker account needs carry enough
//  lamports to cover TX fees (delegation) for one year,
//  and we support one delegation per epoch
fn calculate_staker_fees(genesis_config: &GenesisConfig, years: f64) -> u64 {
    genesis_config.fee_rate_governor.max_lamports_per_signature
        * genesis_config.epoch_schedule.get_epoch(years_as_slots(
            years,
            &genesis_config.poh_config.target_tick_duration,
            genesis_config.ticks_per_slot,
        ) as Slot)
}

/// create stake accounts for lamports with at most stake_granularity in each
///  account
pub fn create_and_add_stakes(
    genesis_config: &mut GenesisConfig,
    // information about this staker for this group of stakes
    staker_info: &StakerInfo,
    // description of how the stakes' lockups will expire
    unlock_info: &UnlockInfo,
    // the largest each stake account should be, in lamports
    granularity: Option<u64>,
) -> u64 {
    let granularity = granularity.unwrap_or(u64::MAX);
    let staker = &staker_info
        .staker
        .parse::<Pubkey>()
        .expect("invalid staker");
    let withdrawer = &staker_info
        .withdrawer
        .unwrap_or(staker_info.staker)
        .parse::<Pubkey>()
        .expect("invalid staker");
    let authorized = Authorized {
        staker: *staker,
        withdrawer: *withdrawer,
    };
    let custodian = unlock_info
        .custodian
        .parse::<Pubkey>()
        .expect("invalid custodian");

    let total_lamports = staker_info.lamports;

    // staker is a system account
    let staker_rent_reserve = genesis_config.rent.minimum_balance(0).max(1);
    let staker_fees = calculate_staker_fees(genesis_config, 1.0);

    let mut stakes_lamports = total_lamports - staker_fees;

    // lamports required to run staking operations for one year
    //  the staker account needs to be rent exempt *and* carry enough
    //  lamports to cover TX fees (delegation) for one year,
    //  and we support one delegation per epoch
    // a single staker may administer any number of accounts
    genesis_config
        .accounts
        .entry(authorized.staker)
        .or_insert_with(|| {
            stakes_lamports -= staker_rent_reserve;
            Account::new(staker_rent_reserve, 0, &system_program::id())
        })
        .lamports += staker_fees;

    // the staker account needs to be rent exempt *and* carry enough
    //  lamports to cover TX fees (delegation) for one year
    //  as we support one re-delegation per epoch
    let unlocks = Unlocks::new(
        unlock_info.cliff_fraction,
        unlock_info.cliff_years,
        unlock_info.unlocks,
        unlock_info.unlock_years,
        &genesis_config.epoch_schedule,
        &genesis_config.poh_config.target_tick_duration,
        genesis_config.ticks_per_slot,
    );

    let mut address_generator = AddressGenerator::new(&authorized.staker, &stake::program::id());

    let stake_rent_reserve = genesis_config.rent.minimum_balance(StakeStateV2::size_of());

    for unlock in unlocks {
        let lamports = unlock.amount(stakes_lamports);

        let (granularity, remainder) = if granularity < lamports {
            (granularity, lamports % granularity)
        } else {
            (lamports, 0)
        };

        let lockup = Lockup {
            epoch: unlock.epoch,
            custodian,
            unix_timestamp: 0,
        };
        for _ in 0..(lamports / granularity).saturating_sub(1) {
            genesis_config.add_account(
                address_generator.next(),
                create_lockup_stake_account(
                    &authorized,
                    &lockup,
                    &genesis_config.rent,
                    granularity,
                ),
            );
        }
        if remainder <= stake_rent_reserve {
            genesis_config.add_account(
                address_generator.next(),
                create_lockup_stake_account(
                    &authorized,
                    &lockup,
                    &genesis_config.rent,
                    granularity + remainder,
                ),
            );
        } else {
            genesis_config.add_account(
                address_generator.next(),
                create_lockup_stake_account(
                    &authorized,
                    &lockup,
                    &genesis_config.rent,
                    granularity,
                ),
            );
            genesis_config.add_account(
                address_generator.next(),
                create_lockup_stake_account(&authorized, &lockup, &genesis_config.rent, remainder),
            );
        }
    }
    total_lamports
}

#[cfg(test)]
mod tests {
    use {super::*, solana_sdk::rent::Rent};

    fn create_and_check_stakes(
        genesis_config: &mut GenesisConfig,
        staker_info: &StakerInfo,
        unlock_info: &UnlockInfo,
        total_lamports: u64,
        granularity: u64,
        len: usize,
    ) {
        assert_eq!(
            total_lamports,
            create_and_add_stakes(genesis_config, staker_info, unlock_info, Some(granularity))
        );
        assert_eq!(genesis_config.accounts.len(), len);
        assert_eq!(
            genesis_config
                .accounts
                .values()
                .map(|account| account.lamports)
                .sum::<u64>(),
            total_lamports,
        );
        assert!(genesis_config
            .accounts
            .iter()
            .all(|(_pubkey, account)| account.lamports <= granularity
                || account.lamports - granularity
                    <= genesis_config.rent.minimum_balance(StakeStateV2::size_of())));
    }

    #[test]
    fn test_create_stakes() {
        // 2 unlocks

        let rent = Rent {
            lamports_per_byte_year: 1,
            exemption_threshold: 1.0,
            ..Rent::default()
        };

        let reserve = rent.minimum_balance(StakeStateV2::size_of());
        let staker_reserve = rent.minimum_balance(0);

        // verify that a small remainder ends up in the last stake
        let granularity = reserve;
        let total_lamports = staker_reserve + reserve * 2 + 1;
        create_and_check_stakes(
            &mut GenesisConfig {
                rent: rent.clone(),
                ..GenesisConfig::default()
            },
            &StakerInfo {
                name: "fun",
                staker: "P1aceHo1derPubkey11111111111111111111111111",
                lamports: total_lamports,
                withdrawer: None,
            },
            &UnlockInfo {
                cliff_fraction: 0.5,
                cliff_years: 0.5,
                unlocks: 1,
                unlock_years: 0.5,
                custodian: "11111111111111111111111111111111",
            },
            total_lamports,
            granularity,
            2 + 1,
        );

        // huge granularity doesn't blow up
        let granularity = u64::MAX;
        let total_lamports = staker_reserve + reserve * 2 + 1;
        create_and_check_stakes(
            &mut GenesisConfig {
                rent: rent.clone(),
                ..GenesisConfig::default()
            },
            &StakerInfo {
                name: "fun",
                staker: "P1aceHo1derPubkey11111111111111111111111111",
                lamports: total_lamports,
                withdrawer: None,
            },
            &UnlockInfo {
                cliff_fraction: 0.5,
                cliff_years: 0.5,
                unlocks: 1,
                unlock_years: 0.5,
                custodian: "11111111111111111111111111111111",
            },
            total_lamports,
            granularity,
            2 + 1,
        );

        // exactly reserve as a remainder, reserve gets folded in
        let granularity = reserve * 3;
        let total_lamports = staker_reserve + (granularity + reserve) * 2;
        create_and_check_stakes(
            &mut GenesisConfig {
                rent: rent.clone(),
                ..GenesisConfig::default()
            },
            &StakerInfo {
                name: "fun",
                staker: "P1aceHo1derPubkey11111111111111111111111111",
                lamports: total_lamports,
                withdrawer: None,
            },
            &UnlockInfo {
                cliff_fraction: 0.5,
                cliff_years: 0.5,
                unlocks: 1,
                unlock_years: 0.5,
                custodian: "11111111111111111111111111111111",
            },
            total_lamports,
            granularity,
            2 + 1,
        );
        // exactly reserve + 1 as a remainder, reserve + 1 gets its own stake
        let granularity = reserve * 3;
        let total_lamports = staker_reserve + (granularity + reserve + 1) * 2;
        create_and_check_stakes(
            &mut GenesisConfig {
                rent: rent.clone(),
                ..GenesisConfig::default()
            },
            &StakerInfo {
                name: "fun",
                staker: "P1aceHo1derPubkey11111111111111111111111111",
                lamports: total_lamports,
                withdrawer: None,
            },
            &UnlockInfo {
                cliff_fraction: 0.5,
                cliff_years: 0.5,
                unlocks: 1,
                unlock_years: 0.5,
                custodian: "11111111111111111111111111111111",
            },
            total_lamports,
            granularity,
            4 + 1,
        );
    }
}
