use {
    crate::bank::Bank,
    log::*,
    solana_account::ReadableAccount,
    solana_accounts_db::accounts_index::{AccountIndex, IndexKey, ScanConfig, ScanResult},
    solana_pubkey::Pubkey,
    solana_stake_interface::{self as stake, state::StakeStateV2},
    solana_stake_program::stake_state,
    std::collections::HashSet,
};

pub struct NonCirculatingSupply {
    pub lamports: u64,
    pub accounts: Vec<Pubkey>,
}

pub fn calculate_non_circulating_supply(bank: &Bank) -> ScanResult<NonCirculatingSupply> {
    debug!("Updating Bank supply, epoch: {}", bank.epoch());
    let mut non_circulating_accounts_set: HashSet<Pubkey> = HashSet::new();

    for key in non_circulating_accounts() {
        non_circulating_accounts_set.insert(key);
    }
    let withdraw_authority_list = withdraw_authority();

    let clock = bank.clock();
    let config = &ScanConfig::default();
    let stake_accounts = if bank
        .rc
        .accounts
        .accounts_db
        .account_indexes
        .contains(&AccountIndex::ProgramId)
    {
        bank.get_filtered_indexed_accounts(
            &IndexKey::ProgramId(stake::program::id()),
            // The program-id account index checks for Account owner on inclusion. However, due to
            // the current AccountsDb implementation, an account may remain in storage as a
            // zero-lamport Account::Default() after being wiped and reinitialized in later
            // updates. We include the redundant filter here to avoid returning these accounts.
            |account| account.owner() == &stake::program::id(),
            config,
            None,
        )?
    } else {
        bank.get_program_accounts(&stake::program::id(), config)?
    };

    for (pubkey, account) in stake_accounts.iter() {
        let stake_account = stake_state::from(account).unwrap_or_default();
        match stake_account {
            StakeStateV2::Initialized(meta) => {
                if meta.lockup.is_in_force(&clock, None)
                    || withdraw_authority_list.contains(&meta.authorized.withdrawer)
                {
                    non_circulating_accounts_set.insert(*pubkey);
                }
            }
            StakeStateV2::Stake(meta, _stake, _stake_flags) => {
                if meta.lockup.is_in_force(&clock, None)
                    || withdraw_authority_list.contains(&meta.authorized.withdrawer)
                {
                    non_circulating_accounts_set.insert(*pubkey);
                }
            }
            _ => {}
        }
    }

    let lamports = non_circulating_accounts_set
        .iter()
        .map(|pubkey| bank.get_balance(pubkey))
        .sum();

    Ok(NonCirculatingSupply {
        lamports,
        accounts: non_circulating_accounts_set.into_iter().collect(),
    })
}

// X1 mainnet accounts that should be considered non-circulating.
// These are foundation/reserve/protocol-controlled accounts, kept in sync with the
// authoritative custody set used by api.x1.xyz/v1/supply/circulating (XLT-137).
pub fn non_circulating_accounts() -> Vec<Pubkey> {
    [
        // Foundation Treasury
        solana_pubkey::pubkey!("7RxMTzi4GfNFvxiJYmgPKPJYBLobVPWh11LQc7rufPG7"),
        // X1 Labs Treasury
        solana_pubkey::pubkey!("2sXr5THr5ZAwSfuJAc1KogPKZ2uKkhfNUAtEvNw5Q6Z7"),
        // Delegation Program PDA
        solana_pubkey::pubkey!("uXgzYh1XhJbeoxNYf55t9E9NhpjC1kWe1mMvpp8vM31"),
        // Authority PDA
        solana_pubkey::pubkey!("DcWrgdX5UDw7ZgKpkJ3KstkGs1Z4DiQwgWhwxKY82eQ9"),
    ]
    .into()
}

// Withdraw authority for autostaked accounts on mainnet-beta
pub fn withdraw_authority() -> Vec<Pubkey> {
    [
        solana_pubkey::pubkey!("8CUUMKYNGxdgYio5CLHRHyzMEhhVRMcqefgE6dLqnVRK"),
        solana_pubkey::pubkey!("3FFaheyqtyAXZSYxDzsr5CVKvJuvZD1WE1VEsBtDbRqB"),
        solana_pubkey::pubkey!("FdGYQdiRky8NZzN9wZtczTBcWLYYRXrJ3LMDhqDPn5rM"),
        solana_pubkey::pubkey!("4e6KwQpyzGQPfgVr5Jn3g5jLjbXB4pKPa2jRLohEb1QA"),
        solana_pubkey::pubkey!("FjiEiVKyMGzSLpqoB27QypukUfyWHrwzPcGNtopzZVdh"),
        solana_pubkey::pubkey!("DwbVjia1mYeSGoJipzhaf4L5hfer2DJ1Ys681VzQm5YY"),
        solana_pubkey::pubkey!("GeMGyvsTEsANVvcT5cme65Xq5MVU8fVVzMQ13KAZFNS2"),
        solana_pubkey::pubkey!("Bj3aQ2oFnZYfNR1njzRjmWizzuhvfcYLckh76cqsbuBM"),
        solana_pubkey::pubkey!("4ZJhPQAgUseCsWhKvJLTmmRRUV74fdoTpQLNfKoekbPY"),
        solana_pubkey::pubkey!("HXdYQ5gixrY2H6Y9gqsD8kPM2JQKSaRiohDQtLbZkRWE"),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::genesis_utils::genesis_sysvar_and_builtin_program_lamports,
        solana_account::{Account, AccountSharedData},
        solana_cluster_type::ClusterType,
        solana_epoch_schedule::EpochSchedule,
        solana_genesis_config::GenesisConfig,
        solana_stake_interface::state::{Authorized, Lockup, Meta},
        std::{collections::BTreeMap, sync::Arc},
    };

    fn new_from_parent(parent: Arc<Bank>) -> Bank {
        let slot = parent.slot() + 1;
        let collector_id = Pubkey::default();
        Bank::new_from_parent(parent, &collector_id, slot)
    }

    #[test]
    fn test_calculate_non_circulating_supply() {
        let mut accounts: BTreeMap<Pubkey, Account> = BTreeMap::new();
        let balance = 10;
        let num_genesis_accounts = 10;
        for _ in 0..num_genesis_accounts {
            accounts.insert(
                solana_pubkey::new_rand(),
                Account::new(balance, 0, &Pubkey::default()),
            );
        }
        let non_circulating_accounts = non_circulating_accounts();
        let num_non_circulating_accounts = non_circulating_accounts.len() as u64;
        for key in non_circulating_accounts.clone() {
            accounts.insert(key, Account::new(balance, 0, &Pubkey::default()));
        }

        let num_stake_accounts = 3;
        for _ in 0..num_stake_accounts {
            let pubkey = solana_pubkey::new_rand();
            let meta = Meta {
                authorized: Authorized::auto(&pubkey),
                lockup: Lockup {
                    epoch: 1,
                    ..Lockup::default()
                },
                ..Meta::default()
            };
            let stake_account = Account::new_data_with_space(
                balance,
                &StakeStateV2::Initialized(meta),
                StakeStateV2::size_of(),
                &stake::program::id(),
            )
            .unwrap();
            accounts.insert(pubkey, stake_account);
        }

        let slots_per_epoch = 32;
        let genesis_config = GenesisConfig {
            accounts,
            epoch_schedule: EpochSchedule::new(slots_per_epoch),
            cluster_type: ClusterType::MainnetBeta,
            ..GenesisConfig::default()
        };
        let mut bank = Arc::new(Bank::new_for_tests(&genesis_config));
        assert_eq!(
            bank.capitalization(),
            (num_genesis_accounts + num_non_circulating_accounts + num_stake_accounts) * balance
                + genesis_sysvar_and_builtin_program_lamports(),
        );

        let non_circulating_supply = calculate_non_circulating_supply(&bank).unwrap();
        assert_eq!(
            non_circulating_supply.lamports,
            (num_non_circulating_accounts + num_stake_accounts) * balance
        );
        assert_eq!(
            non_circulating_supply.accounts.len(),
            num_non_circulating_accounts as usize + num_stake_accounts as usize
        );

        bank = Arc::new(new_from_parent(bank));
        let new_balance = 11;
        for key in non_circulating_accounts {
            bank.store_account(
                &key,
                &AccountSharedData::new(new_balance, 0, &Pubkey::default()),
            );
        }
        let non_circulating_supply = calculate_non_circulating_supply(&bank).unwrap();
        assert_eq!(
            non_circulating_supply.lamports,
            (num_non_circulating_accounts * new_balance) + (num_stake_accounts * balance)
        );
        assert_eq!(
            non_circulating_supply.accounts.len(),
            num_non_circulating_accounts as usize + num_stake_accounts as usize
        );

        // Advance bank an epoch, which should unlock stakes
        for _ in 0..slots_per_epoch {
            bank = Arc::new(new_from_parent(bank));
        }
        assert_eq!(bank.epoch(), 1);
        let non_circulating_supply = calculate_non_circulating_supply(&bank).unwrap();
        assert_eq!(
            non_circulating_supply.lamports,
            num_non_circulating_accounts * new_balance
        );
        assert_eq!(
            non_circulating_supply.accounts.len(),
            num_non_circulating_accounts as usize
        );
    }
}
