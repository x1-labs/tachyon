//! Per-transaction cost of the leader-side vote stake floor.
//!
//! `Consumer::reject_unstaked_vote` is a candidate for the shared block-production
//! chokepoint (`Consumer::process_and_record_transactions_with_pre_results`), where
//! it would run once for every transaction on every lane. This measures that cost
//! against `CostModel::calculate_cost`, the per-transaction work already performed
//! a few lines later in the same function, so the added cost can be read as a
//! fraction of its immediate neighbour rather than as a bare nanosecond count.
//!
//! Transaction shapes are chosen for how deeply they penetrate the predicate:
//! `is_underfunded_vote` delegates first to `solana_fee::is_vote_fee_exempt`,
//! which rejects on signature count, then lookup tables, then instruction count,
//! and only then compares the program id. A vote reaches the epoch-stakes lookup;
//! nothing else does.

use {
    criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main},
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_core::banking_stage::consumer::Consumer,
    solana_cost_model::cost_model::CostModel,
    solana_fee::FeeFeatures,
    solana_hash::Hash,
    solana_keypair::Keypair,
    solana_message::Message,
    solana_runtime::{
        bank::Bank,
        genesis_utils::{ValidatorVoteKeypairs, create_genesis_config_with_vote_accounts},
    },
    solana_runtime_transaction::runtime_transaction::RuntimeTransaction,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction,
    solana_system_transaction as system_transaction,
    solana_transaction::{Transaction, sanitized::SanitizedTransaction},
    solana_vote::vote_transaction::new_tower_sync_transaction,
    solana_vote_program::vote_state::{MAX_LOCKOUT_HISTORY, TowerSync},
    std::{hint::black_box, sync::Arc},
};

/// A batch sized like the ones the scheduler hands to a consume worker.
const BATCH_SIZE: usize = 64;

type Tx = RuntimeTransaction<SanitizedTransaction>;

/// Bank whose epoch stakes contain exactly one staked vote account.
fn setup() -> (Arc<Bank>, ValidatorVoteKeypairs, ValidatorVoteKeypairs) {
    let staked = ValidatorVoteKeypairs::new_rand();
    // Never funded in genesis, so its vote account carries zero activated stake.
    let unstaked = ValidatorVoteKeypairs::new_rand();
    let genesis_config =
        create_genesis_config_with_vote_accounts(1_000_000_000, &[&staked], vec![1_000_000_000])
            .genesis_config;
    let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    (bank, staked, unstaked)
}

/// `lockouts` drives the size of the bincode payload the predicate must
/// deserialize. Real consensus votes run a full tower (31 lockouts), so a
/// single-lockout vote would understate the cost of the path that matters.
fn vote_with_lockouts(keypairs: &ValidatorVoteKeypairs, lockouts: u64) -> Tx {
    let slots: Vec<(u64, u32)> = (0..lockouts)
        .map(|i| (i, lockouts.saturating_sub(i) as u32))
        .collect();
    RuntimeTransaction::from_transaction_for_tests(new_tower_sync_transaction(
        TowerSync::from(slots),
        Hash::default(),
        &keypairs.node_keypair,
        &keypairs.vote_keypair,
        &keypairs.vote_keypair,
        None,
    ))
}

/// A full tower: the shape an actual validator submits every slot.
fn vote(keypairs: &ValidatorVoteKeypairs) -> Tx {
    vote_with_lockouts(keypairs, MAX_LOCKOUT_HISTORY as u64)
}

/// Legacy, one signature, one instruction: penetrates the predicate furthest of
/// any non-vote shape, failing only at the final program-id comparison.
fn transfer() -> Tx {
    let payer = Keypair::new();
    RuntimeTransaction::from_transaction_for_tests(system_transaction::transfer(
        &payer,
        &payer.pubkey(),
        1,
        Hash::default(),
    ))
}

/// The common real-world shape: a compute-budget instruction ahead of the
/// payload, so the predicate exits one check earlier on instruction count.
fn transfer_with_compute_budget() -> Tx {
    let payer = Keypair::new();
    let message = Message::new(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(200_000),
            system_instruction::transfer(&payer.pubkey(), &payer.pubkey(), 1),
        ],
        Some(&payer.pubkey()),
    );
    RuntimeTransaction::from_transaction_for_tests(Transaction::new(
        &[&payer],
        message,
        Hash::default(),
    ))
}

fn bench_vote_admission(c: &mut Criterion) {
    let (bank, staked, unstaked) = setup();

    let batches: [(&str, Vec<Tx>); 5] = [
        ("transfer", (0..BATCH_SIZE).map(|_| transfer()).collect()),
        (
            "transfer_with_compute_budget",
            (0..BATCH_SIZE)
                .map(|_| transfer_with_compute_budget())
                .collect(),
        ),
        (
            "vote_staked_admitted",
            (0..BATCH_SIZE).map(|_| vote(&staked)).collect(),
        ),
        (
            "vote_unstaked_rejected",
            (0..BATCH_SIZE).map(|_| vote(&unstaked)).collect(),
        ),
        // Contrast: a minimal tower isolates how much of the vote-path cost is
        // the bincode payload rather than fixed overhead.
        (
            "vote_single_lockout",
            (0..BATCH_SIZE)
                .map(|_| vote_with_lockouts(&staked, 1))
                .collect(),
        ),
    ];

    // Sanity-check that each batch exercises the branch it is named for; a
    // silently mis-built transaction would benchmark the wrong path.
    for (name, batch) in &batches {
        let rejected = Consumer::reject_unstaked_vote(&batch[0], &bank).is_err();
        assert_eq!(
            rejected,
            *name == "vote_unstaked_rejected",
            "batch `{name}` did not take its intended branch"
        );
    }

    let mut group = c.benchmark_group("vote_admission");
    group.throughput(Throughput::Elements(BATCH_SIZE as u64));

    for (name, batch) in &batches {
        // The candidate: what the chokepoint would add per transaction.
        group.bench_with_input(
            BenchmarkId::new("reject_unstaked_vote", name),
            batch,
            |b, batch| {
                b.iter(|| {
                    for tx in batch {
                        black_box(Consumer::reject_unstaked_vote(tx, &bank).is_ok());
                    }
                })
            },
        );

        // The neighbour: per-transaction work already done in the same function.
        group.bench_with_input(
            BenchmarkId::new("baseline_cost_model", name),
            batch,
            |b, batch| {
                b.iter(|| {
                    for tx in batch {
                        black_box(CostModel::calculate_cost(tx, &bank.feature_set));
                    }
                })
            },
        );
    }

    group.finish();
}

/// Splits the vote-path cost into its two halves. `is_underfunded_vote` is
/// `is_vote_fee_exempt` (which, once `require_vote_submission_for_fee_exemption`
/// is active, bincode-deserializes the vote instruction) followed by an
/// epoch-stakes lookup. Knowing which half dominates decides whether the cost is
/// irreducible or is work the pipeline already performs elsewhere.
fn bench_vote_path_breakdown(c: &mut Criterion) {
    let (bank, staked, _unstaked) = setup();
    let batch: Vec<Tx> = (0..BATCH_SIZE).map(|_| vote(&staked)).collect();
    let fee_features = FeeFeatures::from(bank.feature_set.as_ref());
    let epoch_stakes = bank.current_epoch_stakes();
    let vote_pubkey = staked.vote_keypair.pubkey();

    assert!(
        fee_features.require_vote_submission_for_fee_exemption,
        "benchmark must reflect the gate state live on both X1 chains"
    );

    let mut group = c.benchmark_group("vote_path_breakdown");
    group.throughput(Throughput::Elements(BATCH_SIZE as u64));

    // Half one: the fee-exemption predicate, including the deserialize.
    group.bench_function("is_vote_fee_exempt", |b| {
        b.iter(|| {
            for tx in &batch {
                black_box(solana_fee::is_vote_fee_exempt(tx, fee_features));
            }
        })
    });

    // Half two: the epoch-stakes lookup on its own.
    group.bench_function("vote_account_stake_lookup", |b| {
        b.iter(|| {
            for _ in 0..BATCH_SIZE {
                black_box(epoch_stakes.vote_account_stake(&vote_pubkey));
            }
        })
    });

    group.finish();
}

criterion_group!(benches, bench_vote_admission, bench_vote_path_breakdown);
criterion_main!(benches);
