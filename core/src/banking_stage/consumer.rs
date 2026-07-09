use {
    super::{
        committer::{CommitTransactionDetails, Committer},
        leader_slot_timing_metrics::LeaderExecuteAndCommitTimings,
        qos_service::QosService,
        scheduler_messages::MaxAge,
    },
    itertools::Itertools,
    solana_bincode::limited_deserialize,
    solana_clock::MAX_PROCESSING_AGE,
    solana_fee::FeeFeatures,
    solana_fee_structure::FeeBudgetLimits,
    solana_measure::measure_us,
    solana_metrics::datapoint_info,
    solana_packet::PACKET_DATA_SIZE,
    solana_poh::{
        poh_recorder::PohRecorderError,
        transaction_recorder::{RecordTransactionsTimings, TransactionRecorder},
    },
    solana_runtime::{
        bank::{
            Bank, LoadAndExecuteTransactionsOutput, entry_bytes_budget::EntryBytesReserveError,
        },
        transaction_batch::TransactionBatch,
    },
    solana_runtime_transaction::transaction_with_meta::TransactionWithMeta,
    solana_sdk_ids::vote,
    solana_svm::{
        account_loader::validate_fee_payer,
        transaction_error_metrics::TransactionErrorMetrics,
        transaction_processing_result::TransactionProcessingResultExtensions,
        transaction_processor::{ExecutionRecordingConfig, TransactionProcessingConfig},
    },
    solana_svm_transaction::svm_message::SVMMessage,
    solana_transaction_error::TransactionError,
    solana_vote::vote_parser,
    solana_vote_program::vote_instruction::VoteInstruction,
    std::{cell::Cell, num::Saturating},
};

/// Consumer will create chunks of transactions from buffer with up to this size.
pub const TARGET_NUM_TRANSACTIONS_PER_BATCH: usize = 64;

const SERIALIZED_ENTRIES_OVERHEAD: u64 = {
    48  // Entry Header
    + 8 // Vec<Entry> length
};

#[derive(Debug)]
pub struct ExecutionFlags {
    /// Should failing transactions within the batch be dropped (no fee charged
    /// & not committed).
    pub drop_on_failure: bool,
    /// If any transaction in the batch is not committed then the entire batch
    /// should not be committed.
    ///
    /// # Note
    ///
    /// Without `drop_on_failure` this flag will still allow processed but
    /// failing transactions to be committed. If both flags are set then any
    /// failing transaction will cause all transactions to be aborted.
    pub all_or_nothing: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for ExecutionFlags {
    fn default() -> Self {
        Self {
            drop_on_failure: false,
            all_or_nothing: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetryableIndex {
    pub index: usize,
    pub immediately_retryable: bool,
}

impl RetryableIndex {
    pub fn new(index: usize, immediately_retryable: bool) -> Self {
        Self {
            index,
            immediately_retryable,
        }
    }
}

pub struct ProcessTransactionBatchOutput {
    // The number of transactions filtered out by the cost model
    pub(crate) cost_model_throttled_transactions_count: u64,
    // Amount of time spent running the cost model
    pub(crate) cost_model_us: u64,
    pub execute_and_commit_transactions_output: ExecuteAndCommitTransactionsOutput,
}

pub struct ExecuteAndCommitTransactionsOutput {
    // Transactions counts reported to `ConsumeWorkerMetrics` and then
    // accumulated later for `LeaderSlotMetrics`
    pub(crate) transaction_counts: LeaderProcessedTransactionCounts,
    // Transactions that either were not executed, or were executed and failed to be committed due
    // to the block ending.
    pub(crate) retryable_transaction_indexes: Vec<RetryableIndex>,
    // A result that indicates whether transactions were successfully
    // committed into the Poh stream.
    pub commit_transactions_result: Result<Vec<CommitTransactionDetails>, PohRecorderError>,
    pub(crate) execute_and_commit_timings: LeaderExecuteAndCommitTimings,
    pub(crate) error_counters: TransactionErrorMetrics,
    pub(crate) min_prioritization_fees: u64,
    pub(crate) max_prioritization_fees: u64,
}

#[derive(Debug, Default, PartialEq)]
pub struct LeaderProcessedTransactionCounts {
    // Total number of transactions that were passed as candidates for processing
    pub(crate) attempted_processing_count: u64,
    // The number of transactions of that were processed. See description of in `ProcessTransactionsSummary`
    // for possible outcomes of execution.
    pub(crate) processed_count: u64,
    // Total number of the processed transactions that returned success/not
    // an error.
    pub(crate) processed_with_successful_result_count: u64,
}

pub struct Consumer {
    committer: Committer,
    transaction_recorder: TransactionRecorder,
    qos_service: QosService,
    log_messages_bytes_limit: Option<usize>,
}

impl Consumer {
    pub fn new(
        committer: Committer,
        transaction_recorder: TransactionRecorder,
        qos_service: QosService,
        log_messages_bytes_limit: Option<usize>,
    ) -> Self {
        Self {
            committer,
            transaction_recorder,
            qos_service,
            log_messages_bytes_limit,
        }
    }

    pub fn process_and_record_transactions(
        &self,
        bank: &Bank,
        txs: &[impl TransactionWithMeta],
    ) -> ProcessTransactionBatchOutput {
        let mut error_counters = TransactionErrorMetrics::default();
        let pre_results = vec![Ok(()); txs.len()];
        let check_results = bank.check_transactions(
            txs,
            &pre_results,
            MAX_PROCESSING_AGE,
            true,
            &mut error_counters,
        );
        let check_results: Vec<_> = check_results
            .into_iter()
            .zip(txs.iter())
            .map(|(result, tx)| match result {
                Ok(_) => {
                    if bank.vote_only_bank() && !vote_parser::is_valid_vote_only_transaction(tx) {
                        Err(TransactionError::SanitizeFailure)
                    } else {
                        Ok(())
                    }
                }
                Err(err) => Err(err),
            })
            .collect();
        let mut output = self.process_and_record_transactions_with_pre_results(
            bank,
            txs,
            check_results.into_iter(),
            ExecutionFlags::default(),
        );

        // Accumulate error counters from the initial checks into final results
        output
            .execute_and_commit_transactions_output
            .error_counters
            .accumulate(&error_counters);
        output
    }

    pub fn process_and_record_aged_transactions(
        &self,
        bank: &Bank,
        txs: &[impl TransactionWithMeta],
        max_ages: &[MaxAge],
        flags: ExecutionFlags,
    ) -> ProcessTransactionBatchOutput {
        // Need to filter out transactions since they were sanitized earlier.
        // This means that the transaction may cross and epoch boundary (not allowed),
        //  or account lookup tables may have been closed.
        let pre_results = txs.iter().zip(max_ages).map(|(tx, max_age)| {
            bank.resanitize_transaction_minimally(
                tx,
                max_age.sanitized_epoch,
                max_age.alt_invalidation_slot,
            )
        });
        self.process_and_record_transactions_with_pre_results(bank, txs, pre_results, flags)
    }

    fn process_and_record_transactions_with_pre_results(
        &self,
        bank: &Bank,
        txs: &[impl TransactionWithMeta],
        pre_results: impl Iterator<Item = Result<(), TransactionError>>,
        flags: ExecutionFlags,
    ) -> ProcessTransactionBatchOutput {
        // The vote stake floor is a property of block production, not of the
        // socket a packet arrived on. Every leader path funnels through here —
        // the non-vote scheduler, the vote worker, and the external packer — so
        // applying the floor once at this point covers all of them, and a lane
        // added later cannot silently skip it.
        //
        // Leader-side only: this function is banking stage, never replay, so it
        // cannot change a bank hash. It is ungated and takes effect immediately.
        // Counted rather than inferred: a rejected pre-result is otherwise
        // indistinguishable from genuine cost-model throttling in
        // `cost_model_throttled_transactions_count`. This filter should
        // essentially never fire in normal operation, so a non-zero value is
        // worth seeing. `Cell` is sound here because the iterator is fully
        // consumed by `select_and_accumulate_transaction_costs` below.
        let num_dropped_on_vote_stake_floor = Cell::new(0u64);
        let pre_results = pre_results.zip(txs).map(|(result, tx)| {
            result.and_then(|()| {
                Self::reject_unstaked_vote(tx, bank).inspect_err(|_| {
                    num_dropped_on_vote_stake_floor
                        .set(num_dropped_on_vote_stake_floor.get().saturating_add(1));
                })
            })
        });

        let (
            (transaction_qos_cost_results, cost_model_throttled_transactions_count),
            cost_model_us,
        ) = measure_us!(self.qos_service.select_and_accumulate_transaction_costs(
            bank,
            txs,
            pre_results
        ));

        if num_dropped_on_vote_stake_floor.get() > 0 {
            datapoint_info!(
                "banking_stage-vote_stake_floor",
                ("slot", bank.slot(), i64),
                ("dropped", num_dropped_on_vote_stake_floor.get(), i64)
            );
        }

        // Only lock accounts for those transactions are selected for the block;
        // Once accounts are locked, other threads cannot encode transactions that will modify the
        // same account state
        let (batch, lock_us) = measure_us!(bank.prepare_sanitized_batch_with_results(
            txs,
            transaction_qos_cost_results.iter().map(|r| match r {
                Ok(_cost) => Ok(()),
                Err(err) => Err(err.clone()),
            })
        ));

        // retryable_txs includes AccountInUse, WouldExceedMaxBlockCostLimit
        // WouldExceedMaxAccountCostLimit, WouldExceedMaxVoteCostLimit
        // and WouldExceedMaxAccountDataCostLimit
        let execute_and_commit_transactions_output =
            self.execute_and_commit_transactions_locked(bank, &batch, flags);

        // Once the accounts are new transactions can enter the pipeline to process them
        let (_, unlock_us) = measure_us!(drop(batch));

        let ExecuteAndCommitTransactionsOutput {
            ref commit_transactions_result,
            ..
        } = execute_and_commit_transactions_output;

        // Costs of all transactions are added to the cost_tracker before processing.
        // To ensure accurate tracking of compute units, transactions that ultimately
        // were not included in the block should have their cost removed, the rest
        // should update with their actually consumed units.
        QosService::remove_or_update_costs(
            transaction_qos_cost_results.iter(),
            commit_transactions_result.as_ref().ok(),
            bank,
        );

        // reports qos service stats for this batch
        self.qos_service.report_metrics(bank.slot());

        debug!(
            "bank: {} lock: {}us unlock: {}us txs_len: {}",
            bank.slot(),
            lock_us,
            unlock_us,
            txs.len(),
        );

        ProcessTransactionBatchOutput {
            cost_model_throttled_transactions_count,
            cost_model_us,
            execute_and_commit_transactions_output,
        }
    }

    fn execute_and_commit_transactions_locked(
        &self,
        bank: &Bank,
        batch: &TransactionBatch<impl TransactionWithMeta>,
        flags: ExecutionFlags,
    ) -> ExecuteAndCommitTransactionsOutput {
        let transaction_status_sender_enabled = self.committer.transaction_status_sender_enabled();
        let mut execute_and_commit_timings = LeaderExecuteAndCommitTimings::default();

        let min_max = batch
            .sanitized_transactions()
            .iter()
            .filter_map(|transaction| {
                transaction
                    .compute_budget_instruction_details()
                    .sanitize_and_convert_to_compute_budget_limits(&bank.feature_set)
                    .ok()
                    .map(|limits| limits.compute_unit_price)
            })
            .minmax();
        let (min_prioritization_fees, max_prioritization_fees) =
            min_max.into_option().unwrap_or_default();

        let mut error_counters = TransactionErrorMetrics::default();
        let mut retryable_transaction_indexes: Vec<_> = batch
            .lock_results()
            .iter()
            .enumerate()
            .filter_map(|(index, res)| match res {
                // following are retryable errors
                Err(TransactionError::AccountInUse) => {
                    error_counters.account_in_use += 1;
                    // locking failure due to vote conflict or jito - immediately retry.
                    Some(RetryableIndex {
                        index,
                        immediately_retryable: true,
                    })
                }
                Err(TransactionError::WouldExceedMaxBlockCostLimit) => {
                    error_counters.would_exceed_max_block_cost_limit += 1;
                    Some(RetryableIndex {
                        index,
                        immediately_retryable: false,
                    })
                }
                Err(TransactionError::WouldExceedMaxVoteCostLimit) => {
                    error_counters.would_exceed_max_vote_cost_limit += 1;
                    Some(RetryableIndex {
                        index,
                        immediately_retryable: false,
                    })
                }
                Err(TransactionError::WouldExceedMaxAccountCostLimit) => {
                    error_counters.would_exceed_max_account_cost_limit += 1;
                    Some(RetryableIndex {
                        index,
                        immediately_retryable: false,
                    })
                }
                Err(TransactionError::WouldExceedAccountDataBlockLimit) => {
                    error_counters.would_exceed_account_data_block_limit += 1;
                    Some(RetryableIndex {
                        index,
                        immediately_retryable: false,
                    })
                }
                // following are non-retryable errors
                Err(TransactionError::TooManyAccountLocks) => {
                    error_counters.too_many_account_locks += 1;
                    None
                }
                Err(_) => None,
                Ok(_) => None,
            })
            .collect();

        let (load_and_execute_transactions_output, load_execute_us) =
            measure_us!(bank.load_and_execute_transactions(
                batch,
                MAX_PROCESSING_AGE,
                &mut execute_and_commit_timings.execute_timings,
                &mut error_counters,
                TransactionProcessingConfig {
                    account_overrides: None,
                    check_program_deployment_slot: bank.check_program_deployment_slot(),
                    log_messages_bytes_limit: self.log_messages_bytes_limit,
                    limit_to_load_programs: true,
                    recording_config: ExecutionRecordingConfig::new_single_setting(
                        transaction_status_sender_enabled
                    ),
                    drop_on_failure: flags.drop_on_failure,
                    all_or_nothing: flags.all_or_nothing,
                    strict_nonce_size_check: true,
                }
            ));
        execute_and_commit_timings.load_execute_us = load_execute_us;

        let LoadAndExecuteTransactionsOutput {
            processing_results,
            processed_counts,
            balance_collector,
        } = load_and_execute_transactions_output;

        let actual_execute_time = execute_and_commit_timings
            .execute_timings
            .execute_accessories
            .process_instructions
            .total_us
            .0;
        let actual_executed_cu = processing_results
            .iter()
            .map(|processing_result| {
                processing_result
                    .as_ref()
                    .map_or(0, |pr| pr.executed_units())
            })
            .sum();
        self.qos_service
            .accumulate_actual_execute_cu(actual_executed_cu);
        self.qos_service
            .accumulate_actual_execute_time(actual_execute_time);

        let transaction_counts = LeaderProcessedTransactionCounts {
            processed_count: processed_counts.processed_transactions_count,
            processed_with_successful_result_count: processed_counts
                .processed_with_successful_result_count,
            attempted_processing_count: processing_results.len() as u64,
        };

        let mut entry_bytes = SERIALIZED_ENTRIES_OVERHEAD;
        let (processed_transactions, processing_results_to_transactions_us) = measure_us!(
            processing_results
                .iter()
                .zip(batch.sanitized_transactions())
                .filter_map(|(processing_result, tx)| {
                    if processing_result.was_processed() {
                        entry_bytes += tx.serialized_size() as u64;
                        Some(tx.to_versioned_transaction())
                    } else {
                        None
                    }
                })
                .collect_vec()
        );

        let (freeze_lock, freeze_lock_us) = measure_us!(bank.freeze_lock());
        execute_and_commit_timings.freeze_lock_us = freeze_lock_us;

        let reserved_bytes =
            bank.entry_bytes_budget()
                .reserve(entry_bytes)
                .map_err(|err| match err {
                    EntryBytesReserveError::ExceedsSlotLimit => PohRecorderError::MaxHeightReached,
                });
        let (record_transactions_summary, record_us) = measure_us!(reserved_bytes.map(|_| {
            self.transaction_recorder
                .record_transactions(bank.bank_id(), processed_transactions)
        }));
        execute_and_commit_timings.record_us = record_us;

        let (recording_result, starting_transaction_index) = match record_transactions_summary {
            Ok(summary) => {
                execute_and_commit_timings.record_transactions_timings =
                    RecordTransactionsTimings {
                        processing_results_to_transactions_us: Saturating(
                            processing_results_to_transactions_us,
                        ),
                        ..summary.record_transactions_timings
                    };
                (summary.result, summary.starting_transaction_index)
            }
            Err(err) => (Err(err), None),
        };

        if let Err(recorder_err) = recording_result {
            retryable_transaction_indexes.extend(processing_results.iter().enumerate().filter_map(
                |(index, processing_result)| {
                    processing_result.was_processed().then_some(RetryableIndex {
                        index,
                        immediately_retryable: true, // recording errors are always immediately retryable
                    })
                },
            ));

            // retryable indexes are expected to be sorted - in this case the
            // `extend` can cause that assumption to be violated.
            retryable_transaction_indexes.sort_unstable();

            return ExecuteAndCommitTransactionsOutput {
                transaction_counts,
                retryable_transaction_indexes,
                commit_transactions_result: Err(recorder_err),
                execute_and_commit_timings,
                error_counters,
                min_prioritization_fees,
                max_prioritization_fees,
            };
        }

        let (commit_time_us, commit_transaction_statuses) =
            if processed_counts.processed_transactions_count != 0 {
                self.committer.commit_transactions(
                    batch,
                    processing_results,
                    starting_transaction_index,
                    bank,
                    balance_collector,
                    &mut execute_and_commit_timings,
                    &processed_counts,
                )
            } else {
                (
                    0,
                    processing_results
                        .into_iter()
                        .map(|processing_result| match processing_result {
                            Ok(_) => unreachable!("processed transaction count is 0"),
                            Err(err) => CommitTransactionDetails::NotCommitted(err),
                        })
                        .collect(),
                )
            };

        drop(freeze_lock);

        debug!(
            "bank: {} process_and_record_locked: {}us record: {}us commit: {}us txs_len: {}",
            bank.slot(),
            load_execute_us,
            record_us,
            commit_time_us,
            batch.sanitized_transactions().len(),
        );

        debug!(
            "execute_and_commit_transactions_locked: {:?}",
            execute_and_commit_timings.execute_timings,
        );

        debug_assert_eq!(
            transaction_counts.attempted_processing_count,
            commit_transaction_statuses.len() as u64,
        );

        ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            retryable_transaction_indexes,
            commit_transactions_result: Ok(commit_transaction_statuses),
            execute_and_commit_timings,
            error_counters,
            min_prioritization_fees,
            max_prioritization_fees,
        }
    }

    /// Reject a transaction that is riding the vote fee exemption while not being a
    /// genuine vote submission (Vote, VoteSwitch, TowerSync, ...).
    ///
    /// A transaction is only rejected when it would be charged a zero fee *because*
    /// it qualifies for the vote exemption (`is_vote_fee_exempt`) yet contains a
    /// non-submission vote-program instruction (Withdraw, Authorize,
    /// UpdateCommission, InitializeAccount, ...). If the transaction instead pays a
    /// normal fee it is allowed through — including once
    /// `require_vote_submission_for_fee_exemption` is active, after which these
    /// administrative instructions are no longer exempt and are charged like any
    /// other transaction. Multi-instruction vote-account operations (e.g.
    /// CreateVoteAccount) are never exempt, so they always pass.
    ///
    /// This is a leader-side block-production filter only: replay/validation is
    /// unchanged, so there is no fork risk.
    pub fn reject_non_vote_submission(
        transaction: &impl SVMMessage,
        fee_features: FeeFeatures,
    ) -> Result<(), TransactionError> {
        // Only transactions that actually receive the vote fee exemption are
        // candidates for rejection; anything paying a normal fee is allowed.
        if !solana_fee::is_vote_fee_exempt(transaction, fee_features) {
            return Ok(());
        }
        for (program_id, instruction) in transaction.program_instructions_iter() {
            if program_id == &vote::ID {
                match limited_deserialize::<VoteInstruction>(
                    instruction.data,
                    PACKET_DATA_SIZE as u64,
                ) {
                    Ok(ref vote_instruction) if vote_instruction.is_simple_vote() => {}
                    _ => return Err(TransactionError::InvalidProgramForExecution),
                }
            }
        }
        Ok(())
    }

    /// Reject a fee-exempt vote submission whose vote account is below the
    /// stake-admission floor.
    ///
    /// The minimum-stake floor
    /// ([`min_vote_account_stake`](solana_runtime::vote_admission::min_vote_account_stake))
    /// is applied by `VoteStorage`, which only sees votes arriving on the
    /// gossip / TPU-vote lane. A genuine `Vote` / `TowerSync` can also reach the
    /// leader through the ordinary transaction (RPC / TPU) lane, which is served
    /// by a different receiver and never consulted the floor. Admission was
    /// therefore a function of which socket a vote arrived on rather than of the
    /// vote account itself. This applies the identical floor so a vote must
    /// clear the same stake bar no matter which port it entered on.
    ///
    /// It is applied in
    /// [`Self::process_and_record_transactions_with_pre_results`], the one
    /// function every leader path funnels through — the non-vote scheduler
    /// (`consume_worker`), the vote worker, and the external packer — so no
    /// lane, present or future, can reach a block without clearing the floor.
    ///
    /// Only transactions that actually receive the vote fee exemption are
    /// candidates: a vote-program transaction that pays a normal fee carries a
    /// real cost and is left alone.
    ///
    /// This is a leader-side block-production filter only: it is ungated, takes
    /// effect immediately, and never changes replay, so there is no fork risk. It
    /// shares [`is_underfunded_vote`](solana_runtime::vote_admission::is_underfunded_vote)
    /// with the gated consensus rule
    /// ([`reject_underfunded_vote_in_consensus`](solana_runtime::vote_admission::reject_underfunded_vote_in_consensus)),
    /// so an honest leader never admits a vote that consensus would then reject.
    pub fn reject_unstaked_vote(
        transaction: &impl SVMMessage,
        bank: &Bank,
    ) -> Result<(), TransactionError> {
        if solana_runtime::vote_admission::is_underfunded_vote(
            transaction,
            bank.current_epoch_stakes(),
            bank.cluster_type(),
            bank.feature_set.as_ref(),
        ) {
            return Err(TransactionError::InvalidProgramForExecution);
        }
        Ok(())
    }

    /// Keep fee-exempt vote submissions off the non-vote (RPC / TPU-transaction)
    /// lane so that every block-included vote flows through the vote-buffer lane.
    ///
    /// Vote submissions have a dedicated ingress path: the vote-buffer lane
    /// (TPU-vote port + gossip), where they are deduplicated per vote account and
    /// admitted against the stake floor. The non-vote lane has neither step, so
    /// handling votes here would route them through a second, inconsistent path.
    /// Validators already submit votes on the TPU-vote port (see
    /// `voting_service`), not via `sendTransaction`, so nothing legitimate relies
    /// on votes reaching this lane.
    ///
    /// The predicate is [`solana_fee::is_vote_fee_exempt`] — the same check the
    /// fee calculator uses to zero a vote's fee, so this matches exactly the
    /// transactions that pay no fee, and nothing else. Vote-program
    /// administration (`Withdraw`, `Authorize`, `UpdateCommission`, …) pays a
    /// normal fee and is unaffected.
    ///
    /// It is deliberately scoped to the non-vote lane and must NOT be moved to
    /// the shared record chokepoint: the vote lane carries genuine consensus
    /// votes, which are fee-exempt by design, so applying this predicate there
    /// would stop the leader including any votes at all. The floor
    /// ([`Self::reject_unstaked_vote`]) is the predicate that is safe on every
    /// lane; this one is strictly broader and strictly lane-local.
    ///
    /// This is a leader-side block-production filter only: it never runs in
    /// replay, so it cannot change a bank hash. It is ungated. On this lane it
    /// is strictly broader than [`Self::reject_unstaked_vote`] — it drops every
    /// fee-exempt vote, not only below-floor ones — but it does not replace it:
    /// the floor is enforced separately at the shared record chokepoint, where
    /// it also covers the vote lane and the external packer.
    pub fn reject_vote_fee_exempt(
        transaction: &impl SVMMessage,
        fee_features: FeeFeatures,
    ) -> Result<(), TransactionError> {
        if solana_fee::is_vote_fee_exempt(transaction, fee_features) {
            return Err(TransactionError::InvalidProgramForExecution);
        }
        Ok(())
    }

    pub fn check_fee_payer_unlocked(
        bank: &Bank,
        transaction: &impl TransactionWithMeta,
        error_counters: &mut TransactionErrorMetrics,
    ) -> Result<(), TransactionError> {
        let fee_payer = transaction.fee_payer();
        let fee_budget_limits = FeeBudgetLimits::from(
            transaction
                .compute_budget_instruction_details()
                .sanitize_and_convert_to_compute_budget_limits(&bank.feature_set)?,
        );
        let fee = solana_fee::calculate_fee(
            transaction,
            bank.get_lamports_per_signature() == 0,
            bank.fee_structure().lamports_per_signature,
            fee_budget_limits.prioritization_fee,
            FeeFeatures::from(bank.feature_set.as_ref()),
        );
        let (mut fee_payer_account, _slot) = bank
            .rc
            .accounts
            .load_with_fixed_root(&bank.ancestors, fee_payer)
            .ok_or(TransactionError::AccountNotFound)?;

        validate_fee_payer(
            fee_payer,
            &mut fee_payer_account,
            0,
            error_counters,
            &bank.rent_collector().rent,
            fee,
        )
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::banking_stage::tests::{create_slow_genesis_config, sanitize_transactions},
        agave_reserved_account_keys::ReservedAccountKeys,
        crossbeam_channel::unbounded,
        solana_account::{AccountSharedData, state_traits::StateMut},
        solana_address_lookup_table_interface::{
            self as address_lookup_table,
            state::{AddressLookupTable, LookupTableMeta},
        },
        solana_cost_model::{cost_model::CostModel, transaction_cost::TransactionCost},
        solana_fee_calculator::FeeCalculator,
        solana_hash::Hash,
        solana_instruction::error::InstructionError,
        solana_keypair::Keypair,
        solana_ledger::{
            blockstore_processor::{TransactionStatusMessage, TransactionStatusSender},
            genesis_utils::{
                GenesisConfigInfo, bootstrap_validator_stake_lamports,
                create_genesis_config_with_leader,
            },
        },
        solana_message::{
            MessageHeader, VersionedMessage,
            v0::{self, MessageAddressTableLookup},
        },
        solana_nonce::{self as nonce, state::DurableNonce},
        solana_nonce_account::verify_nonce_account,
        solana_poh::record_channels::{RecordReceiver, record_channels},
        solana_pubkey::Pubkey,
        solana_runtime::bank_forks::BankForks,
        solana_runtime_transaction::runtime_transaction::RuntimeTransaction,
        solana_signer::Signer,
        solana_system_interface::program as system_program,
        solana_system_transaction as system_transaction,
        solana_transaction::{
            Transaction, sanitized::MessageHash, versioned::VersionedTransaction,
        },
        std::{
            borrow::Cow,
            slice,
            sync::{Arc, RwLock},
        },
        test_case::test_case,
    };

    struct TestFrame {
        mint_keypair: Keypair,
        bank: Arc<Bank>,
        bank_forks: Arc<RwLock<BankForks>>,
        record_receiver: RecordReceiver,
        consumer: Consumer,
    }

    fn setup_test(
        relax_intrabatch_account_locks: bool,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> TestFrame {
        agave_logger::setup();
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config_with_leader(
            10_000,
            &Pubkey::new_unique(),
            bootstrap_validator_stake_lamports(),
        );
        let mut bank = Bank::new_for_tests(&genesis_config);
        if !relax_intrabatch_account_locks {
            bank.deactivate_feature(&agave_feature_set::relax_intrabatch_account_locks::id());
        }
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();

        let (record_sender, mut record_receiver) = record_channels(false);
        let recorder = TransactionRecorder::new(record_sender);
        record_receiver.restart(bank.bank_id());

        let (replay_vote_sender, _replay_vote_receiver) = unbounded();
        let committer = Committer::new(transaction_status_sender, replay_vote_sender, None);
        let consumer = Consumer::new(committer, recorder, QosService::new(1), None);

        TestFrame {
            mint_keypair,
            bank,
            bank_forks,
            record_receiver,
            consumer,
        }
    }

    #[test]
    fn test_reject_non_vote_submission() {
        use {
            solana_vote::vote_transaction,
            solana_vote_program::{
                vote_instruction,
                vote_state::{TowerSync, VoteAuthorize},
            },
        };

        // Legacy exemption (vote-submission opcode gate inactive): a single
        // vote-program instruction is fee-exempt.
        let gate_off = FeeFeatures {
            enable_secp256r1_precompile: true,
            validate_fee_vote_transaction_instructions: true,
            enforce_minimum_transaction_fee: false,
            require_vote_submission_for_fee_exemption: false,
        };
        // Opcode gate active: only genuine submissions are fee-exempt; everything
        // else pays a normal fee.
        let gate_on = FeeFeatures {
            require_vote_submission_for_fee_exemption: true,
            ..gate_off
        };

        let payer = Keypair::new();
        let node = Keypair::new();
        let vote_keypair = Keypair::new();
        let authority = Keypair::new();

        // A plain transfer carries no vote-program instruction -> always allowed.
        let transfer = RuntimeTransaction::from_transaction_for_tests(
            system_transaction::transfer(&payer, &payer.pubkey(), 1, Hash::default()),
        );
        assert!(Consumer::reject_non_vote_submission(&transfer, gate_off).is_ok());
        assert!(Consumer::reject_non_vote_submission(&transfer, gate_on).is_ok());

        // A genuine TowerSync vote submission is exempt and legitimate -> always allowed.
        let tower_sync = RuntimeTransaction::from_transaction_for_tests(
            vote_transaction::new_tower_sync_transaction(
                TowerSync::from(vec![(42, 1)]),
                Hash::default(),
                &node,
                &vote_keypair,
                &authority,
                None,
            ),
        );
        assert!(Consumer::reject_non_vote_submission(&tower_sync, gate_off).is_ok());
        assert!(Consumer::reject_non_vote_submission(&tower_sync, gate_on).is_ok());

        // A single Vote::Withdraw: rejected only while it rides the exemption
        // (gate off). Once the gate makes it pay (gate on), it is allowed.
        let withdraw =
            RuntimeTransaction::from_transaction_for_tests(Transaction::new_signed_with_payer(
                &[vote_instruction::withdraw(
                    &vote_keypair.pubkey(),
                    &authority.pubkey(),
                    1,
                    &Pubkey::new_unique(),
                )],
                Some(&authority.pubkey()),
                &[&authority],
                Hash::default(),
            ));
        assert!(Consumer::reject_non_vote_submission(&withdraw, gate_off).is_err());
        assert!(Consumer::reject_non_vote_submission(&withdraw, gate_on).is_ok());

        // A single Vote::Authorize: same as withdraw.
        let authorize =
            RuntimeTransaction::from_transaction_for_tests(Transaction::new_signed_with_payer(
                &[vote_instruction::authorize(
                    &vote_keypair.pubkey(),
                    &authority.pubkey(),
                    &Pubkey::new_unique(),
                    VoteAuthorize::Voter,
                )],
                Some(&authority.pubkey()),
                &[&authority],
                Hash::default(),
            ));
        assert!(Consumer::reject_non_vote_submission(&authorize, gate_off).is_err());
        assert!(Consumer::reject_non_vote_submission(&authorize, gate_on).is_ok());

        // A multi-instruction vote-account operation is never fee-exempt (more than
        // one instruction), so it pays a normal fee and is allowed regardless of gate.
        let multi =
            RuntimeTransaction::from_transaction_for_tests(Transaction::new_signed_with_payer(
                &[
                    vote_instruction::withdraw(
                        &vote_keypair.pubkey(),
                        &authority.pubkey(),
                        1,
                        &Pubkey::new_unique(),
                    ),
                    vote_instruction::authorize(
                        &vote_keypair.pubkey(),
                        &authority.pubkey(),
                        &Pubkey::new_unique(),
                        VoteAuthorize::Voter,
                    ),
                ],
                Some(&authority.pubkey()),
                &[&authority],
                Hash::default(),
            ));
        assert!(Consumer::reject_non_vote_submission(&multi, gate_off).is_ok());
        assert!(Consumer::reject_non_vote_submission(&multi, gate_on).is_ok());
    }

    #[test]
    fn test_reject_unstaked_vote() {
        // A genuine, fee-exempt vote from a zero-stake vote account must be
        // dropped on the non-vote (RPC / TPU) lane, the same way the vote-buffer
        // lane drops it. A staked vote account is admitted; a non-vote
        // transaction is never affected.
        use {
            solana_cluster_type::ClusterType,
            solana_runtime::genesis_utils::{self, ValidatorVoteKeypairs},
            solana_vote::vote_transaction::new_tower_sync_transaction,
            solana_vote_program::vote_state::TowerSync,
        };

        // `staked` has real activated stake in genesis; `unfunded` exists only
        // as an off-genesis keypair, so its vote account has zero activated
        // stake.
        let staked = ValidatorVoteKeypairs::new_rand();
        let unfunded = ValidatorVoteKeypairs::new_rand();
        let genesis_config = genesis_utils::create_genesis_config_with_vote_accounts(
            1_000_000_000,
            &[&staked],
            vec![1_000_000_000],
        )
        .genesis_config;
        let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        // Development cluster -> floor is the ungated default (MUST_BE_STAKED,
        // i.e. any non-zero stake).
        assert_eq!(bank.cluster_type(), ClusterType::Development);

        let vote_from = |keypairs: &ValidatorVoteKeypairs| {
            RuntimeTransaction::from_transaction_for_tests(new_tower_sync_transaction(
                TowerSync::from(vec![(0, 1)]),
                Hash::default(),
                &keypairs.node_keypair,
                &keypairs.vote_keypair,
                &keypairs.vote_keypair,
                None,
            ))
        };

        // Staked vote account clears the floor -> admitted.
        assert!(Consumer::reject_unstaked_vote(&vote_from(&staked), &bank).is_ok());

        // Zero-stake vote account -> rejected on this lane, matching the
        // vote-buffer lane's behaviour.
        assert!(
            Consumer::reject_unstaked_vote(&vote_from(&unfunded), &bank).is_err(),
            "a zero-stake vote account must be rejected on the non-vote lane"
        );

        // A non-vote transaction is not fee-exempt and is never a candidate.
        let payer = Keypair::new();
        let transfer = RuntimeTransaction::from_transaction_for_tests(
            system_transaction::transfer(&payer, &payer.pubkey(), 1, Hash::default()),
        );
        assert!(Consumer::reject_unstaked_vote(&transfer, &bank).is_ok());
    }

    #[test]
    fn test_reject_vote_fee_exempt() {
        // The non-vote-lane filter drops EVERY fee-exempt vote — even a genuine,
        // fully-staked submission — because genuine votes belong on the
        // vote-buffer lane. It rejects exactly the set that pays no fee, and
        // leaves fee-paying vote-program administration alone.
        use {
            solana_vote::vote_transaction,
            solana_vote_program::{vote_instruction, vote_state::TowerSync},
        };

        // Live X1 config: shape validation + submission-opcode gate both active.
        let gate_on = FeeFeatures {
            enable_secp256r1_precompile: true,
            validate_fee_vote_transaction_instructions: true,
            enforce_minimum_transaction_fee: true,
            require_vote_submission_for_fee_exemption: true,
        };
        // Legacy: submission-opcode gate inactive, so any single vote-program
        // instruction (including Withdraw) rides the exemption.
        let gate_off = FeeFeatures {
            require_vote_submission_for_fee_exemption: false,
            ..gate_on
        };

        let payer = Keypair::new();
        let node = Keypair::new();
        let vote_keypair = Keypair::new();
        let authority = Keypair::new();

        // A genuine, fee-exempt vote submission -> dropped on this lane, staked
        // or not. This is the whole point: no votes over the non-vote lane.
        let tower_sync = RuntimeTransaction::from_transaction_for_tests(
            vote_transaction::new_tower_sync_transaction(
                TowerSync::from(vec![(42, 1)]),
                Hash::default(),
                &node,
                &vote_keypair,
                &authority,
                None,
            ),
        );
        assert!(
            Consumer::reject_vote_fee_exempt(&tower_sync, gate_on).is_err(),
            "a genuine fee-exempt vote must be rejected on the non-vote lane"
        );

        // A plain transfer pays a fee -> never a candidate.
        let transfer = RuntimeTransaction::from_transaction_for_tests(
            system_transaction::transfer(&payer, &payer.pubkey(), 1, Hash::default()),
        );
        assert!(Consumer::reject_vote_fee_exempt(&transfer, gate_on).is_ok());

        // Vote-account administration (Withdraw) pays a normal fee under the live
        // gate -> allowed. Only when it rides the legacy exemption (gate off) is
        // it fee-exempt, and then it is correctly dropped too.
        let withdraw =
            RuntimeTransaction::from_transaction_for_tests(Transaction::new_signed_with_payer(
                &[vote_instruction::withdraw(
                    &vote_keypair.pubkey(),
                    &authority.pubkey(),
                    1,
                    &Pubkey::new_unique(),
                )],
                Some(&authority.pubkey()),
                &[&authority],
                Hash::default(),
            ));
        assert!(
            Consumer::reject_vote_fee_exempt(&withdraw, gate_on).is_ok(),
            "fee-paying vote administration must pass"
        );
        assert!(Consumer::reject_vote_fee_exempt(&withdraw, gate_off).is_err());
    }

    #[test]
    fn test_reject_underfunded_vote_in_consensus() {
        // The consensus rule rejects a fee-exempt vote from a below-floor vote
        // account, but ONLY once a `vote_min_stake_*` gate is
        // active (the switch), so deploying the binary is bank-hash neutral until
        // a coordinated activation. It shares the underfunded-vote predicate with
        // the leader-side filter above, so the two never disagree.
        use {
            agave_feature_set::{
                vote_min_stake_1_xnt, vote_min_stake_10_xnt, vote_min_stake_100_xnt,
            },
            solana_cluster_type::ClusterType,
            solana_runtime::{
                genesis_utils::{self, ValidatorVoteKeypairs},
                vote_admission::reject_underfunded_vote_in_consensus,
            },
            solana_transaction_error::TransactionError,
            solana_vote::vote_transaction::new_tower_sync_transaction,
            solana_vote_program::vote_state::TowerSync,
        };

        let staked = ValidatorVoteKeypairs::new_rand();
        let unfunded = ValidatorVoteKeypairs::new_rand();
        let genesis_config = genesis_utils::create_genesis_config_with_vote_accounts(
            1_000_000_000,
            &[&staked],
            vec![1_000_000_000],
        )
        .genesis_config;
        let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        assert_eq!(bank.cluster_type(), ClusterType::Development);

        let vote_from = |keypairs: &ValidatorVoteKeypairs| {
            RuntimeTransaction::from_transaction_for_tests(new_tower_sync_transaction(
                TowerSync::from(vec![(0, 1)]),
                Hash::default(),
                &keypairs.node_keypair,
                &keypairs.vote_keypair,
                &keypairs.vote_keypair,
                None,
            ))
        };

        let epoch_stakes = bank.current_epoch_stakes();
        let cluster_type = bank.cluster_type();

        // The test bank activates all features, so its feature set already carries
        // the `vote_min_stake_*` gates: the consensus switch is ON. On a
        // development cluster the floor stays MUST_BE_STAKED, so the zero-stake
        // vote is invalid in consensus while the staked vote is accepted.
        let on = bank.feature_set.as_ref();
        assert_eq!(
            reject_underfunded_vote_in_consensus(
                &vote_from(&unfunded),
                epoch_stakes,
                cluster_type,
                on
            ),
            Err(TransactionError::InvalidProgramForExecution),
            "a below-floor vote must be invalid in consensus once gated"
        );
        assert!(
            reject_underfunded_vote_in_consensus(
                &vote_from(&staked),
                epoch_stakes,
                cluster_type,
                on
            )
            .is_ok(),
            "a staked vote must remain valid in consensus"
        );

        // With every `vote_min_stake_*` gate deactivated the switch is OFF, so the
        // rule is inert even for the zero-stake vote — deploying the binary is bank-hash
        // neutral until a gate is activated.
        let mut off = bank.feature_set.as_ref().clone();
        off.deactivate(&vote_min_stake_1_xnt::id());
        off.deactivate(&vote_min_stake_10_xnt::id());
        off.deactivate(&vote_min_stake_100_xnt::id());
        assert!(
            reject_underfunded_vote_in_consensus(
                &vote_from(&unfunded),
                epoch_stakes,
                cluster_type,
                &off
            )
            .is_ok(),
            "the consensus rule must be inert until a vote_min_stake gate activates"
        );
    }

    fn execute_transactions_for_test(
        bank: Arc<Bank>,
        transactions: Vec<Transaction>,
    ) -> ProcessTransactionBatchOutput {
        let transactions = sanitize_transactions(transactions);

        let (record_sender, mut record_receiver) = record_channels(false);
        let recorder = TransactionRecorder::new(record_sender);
        record_receiver.restart(bank.bank_id());

        let (replay_vote_sender, _replay_vote_receiver) = unbounded();
        let committer = Committer::new(None, replay_vote_sender, None);
        let consumer = Consumer::new(committer, recorder, QosService::new(1), None);
        consumer.process_and_record_transactions(&bank, &transactions)
    }

    fn generate_new_address_lookup_table(
        authority: Option<Pubkey>,
        num_addresses: usize,
    ) -> AddressLookupTable<'static> {
        let mut addresses = Vec::with_capacity(num_addresses);
        addresses.resize_with(num_addresses, Pubkey::new_unique);
        AddressLookupTable {
            meta: LookupTableMeta {
                authority,
                ..LookupTableMeta::default()
            },
            addresses: Cow::Owned(addresses),
        }
    }

    fn store_nonce_account(
        bank: &Bank,
        account_address: Pubkey,
        nonce_state: nonce::state::State,
    ) -> AccountSharedData {
        let mut account =
            AccountSharedData::new(1, nonce::state::State::size(), &system_program::id());
        account
            .set_state(&nonce::versions::Versions::new(nonce_state))
            .unwrap();
        bank.store_account(&account_address, &account);

        account
    }

    fn store_address_lookup_table(
        bank: &Bank,
        account_address: Pubkey,
        address_lookup_table: AddressLookupTable<'static>,
    ) -> AccountSharedData {
        let data = address_lookup_table.serialize_for_tests().unwrap();
        let mut account =
            AccountSharedData::new(1, data.len(), &address_lookup_table::program::id());
        account.set_data(data);
        bank.store_account(&account_address, &account);

        account
    }

    #[test]
    fn test_process_and_record_transactions_rejects_unstaked_vote() {
        // The stake floor must be a property of block production, not of the
        // lane a packet happened to arrive on.
        // `process_and_record_transactions_with_pre_results` is the single
        // funnel every leader path reaches -- the non-vote scheduler, the vote
        // worker and the external packer -- so a below-floor fee-exempt vote
        // must be excluded there even when no lane-specific filter ran.
        use {
            solana_runtime::genesis_utils::ValidatorVoteKeypairs,
            solana_vote::vote_transaction::new_tower_sync_transaction,
            solana_vote_program::vote_state::{self, TowerSync, VoteStateV4},
        };

        agave_logger::setup();
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config_with_leader(
            10_000,
            &Pubkey::new_unique(),
            bootstrap_validator_stake_lamports(),
        );
        let mut bank = Bank::new_for_tests(&genesis_config);
        // Test banks activate every feature, which would switch on the *gated
        // consensus* rule and reject the vote before the leader-side filter is
        // ever reached. Deactivating the three gates reproduces the state both
        // X1 chains are actually in, so this exercises block production alone;
        // the floor then falls back to MUST_BE_STAKED_LAMPORTS.
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_1_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_10_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_100_xnt::id());
        let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();
        assert!(
            !solana_runtime::vote_admission::consensus_floor_enforced(bank.feature_set.as_ref()),
            "the consensus rule must be inactive so this isolates the leader-side filter"
        );

        let (record_sender, mut record_receiver) = record_channels(false);
        let recorder = TransactionRecorder::new(record_sender);
        record_receiver.restart(bank.bank_id());
        let (replay_vote_sender, _replay_vote_receiver) = unbounded();
        let committer = Committer::new(None, replay_vote_sender, None);
        let consumer = Consumer::new(committer, recorder, QosService::new(1), None);

        // Never present in genesis, so this vote account has zero activated
        // stake and sits below even the ungated one-lamport floor.
        let unstaked = ValidatorVoteKeypairs::new_rand();
        // Fund the fee payer so the transaction is rejected by the floor rather
        // than by an unfundable payer.
        bank.transfer(
            bank.get_minimum_balance_for_rent_exemption(0),
            &mint_keypair,
            &unstaked.node_keypair.pubkey(),
        )
        .unwrap();

        // Install a real, rent-funded vote account that simply carries no
        // delegated stake. Storing it does not enter it into epoch stakes, so
        // its activated stake is zero while the account itself loads and
        // executes normally.
        // Without this the transaction would fail to load its accounts and the
        // test would pass for a reason unrelated to the floor.
        let vote_pubkey = unstaked.vote_keypair.pubkey();
        bank.store_account(
            &vote_pubkey,
            &vote_state::create_v4_account_with_authorized(
                &unstaked.node_keypair.pubkey(),
                &vote_pubkey,
                unstaked.bls_keypair.public.to_bytes_compressed(),
                &vote_pubkey,
                0,
                &vote_pubkey,
                0,
                &vote_pubkey,
                bank.get_minimum_balance_for_rent_exemption(VoteStateV4::size_of()),
            ),
        );
        assert_eq!(
            bank.current_epoch_stakes().vote_account_stake(&vote_pubkey),
            0,
            "the vote account must exist yet hold no activated stake"
        );

        let vote = sanitize_transactions(vec![new_tower_sync_transaction(
            TowerSync::from(vec![(bank.slot().saturating_sub(1), 1)]),
            bank.confirmed_last_blockhash(),
            &unstaked.node_keypair,
            &unstaked.vote_keypair,
            &unstaked.vote_keypair,
            None,
        )]);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts, ..
        } = consumer
            .process_and_record_transactions(&bank, &vote)
            .execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 1,
                // Excluded before execution by the stake floor.
                processed_count: 0,
                processed_with_successful_result_count: 0,
            },
            "a below-floor fee-exempt vote must not reach execution at the shared chokepoint"
        );
    }

    #[test]
    fn test_process_and_record_transactions_admits_staked_vote() {
        // The counterpart guard to the floor: a genuine vote from a properly
        // staked validator must still reach the block. The chokepoint filter
        // runs on every lane including the vote lane, so an over-broad
        // predicate here would stop the leader including consensus votes at
        // all -- a far worse failure than the one being fixed.
        use {
            solana_runtime::genesis_utils::{
                ValidatorVoteKeypairs, create_genesis_config_with_vote_accounts,
            },
            solana_vote::vote_transaction::new_tower_sync_transaction,
            solana_vote_program::vote_state::TowerSync,
        };

        agave_logger::setup();
        let staked = ValidatorVoteKeypairs::new_rand();
        let genesis_config = create_genesis_config_with_vote_accounts(
            1_000_000_000,
            &[&staked],
            vec![1_000_000_000],
        )
        .genesis_config;
        let mut bank = Bank::new_for_tests(&genesis_config);
        // Same gate state as the rejection test, so the two differ only in
        // whether the vote account carries stake.
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_1_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_10_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_100_xnt::id());
        let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();

        let vote_pubkey = staked.vote_keypair.pubkey();
        assert!(
            bank.current_epoch_stakes().vote_account_stake(&vote_pubkey) > 0,
            "this vote account must carry activated stake for the test to mean anything"
        );

        let (record_sender, mut record_receiver) = record_channels(false);
        let recorder = TransactionRecorder::new(record_sender);
        record_receiver.restart(bank.bank_id());
        let (replay_vote_sender, _replay_vote_receiver) = unbounded();
        let committer = Committer::new(None, replay_vote_sender, None);
        let consumer = Consumer::new(committer, recorder, QosService::new(1), None);

        let vote = sanitize_transactions(vec![new_tower_sync_transaction(
            TowerSync::from(vec![(bank.slot().saturating_sub(1), 1)]),
            bank.confirmed_last_blockhash(),
            &staked.node_keypair,
            &staked.vote_keypair,
            &staked.vote_keypair,
            None,
        )]);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts, ..
        } = consumer
            .process_and_record_transactions(&bank, &vote)
            .execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts.processed_count, 1,
            "a staked validator's vote must still be processed into the block"
        );
    }

    #[test]
    fn test_process_and_record_aged_transactions_rejects_unstaked_vote_in_bundle() {
        // Covers the path the consume workers actually use
        // (`process_and_record_aged_transactions`), including the external
        // packer at `consume_worker.rs:409`, which is the lane that has no
        // filter of its own and so justified putting the floor at the shared
        // chokepoint. The packer also supplies non-default `ExecutionFlags`,
        // so this pins down what an injected floor rejection does to an
        // `all_or_nothing` bundle.
        use {
            crate::banking_stage::scheduler_messages::MaxAge,
            solana_runtime::genesis_utils::ValidatorVoteKeypairs,
            solana_vote::vote_transaction::new_tower_sync_transaction,
            solana_vote_program::vote_state::{self, TowerSync, VoteStateV4},
        };

        agave_logger::setup();
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config_with_leader(
            10_000,
            &Pubkey::new_unique(),
            bootstrap_validator_stake_lamports(),
        );
        let mut bank = Bank::new_for_tests(&genesis_config);
        // Gates off, so only the leader-side floor can reject — see
        // `test_process_and_record_transactions_rejects_unstaked_vote`.
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_1_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_10_xnt::id());
        bank.deactivate_feature(&agave_feature_set::vote_min_stake_100_xnt::id());
        let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();

        let (record_sender, mut record_receiver) = record_channels(false);
        let recorder = TransactionRecorder::new(record_sender);
        record_receiver.restart(bank.bank_id());
        let (replay_vote_sender, _replay_vote_receiver) = unbounded();
        let committer = Committer::new(None, replay_vote_sender, None);
        let consumer = Consumer::new(committer, recorder, QosService::new(1), None);

        let rent_exempt = bank.get_minimum_balance_for_rent_exemption(0);
        let unstaked = ValidatorVoteKeypairs::new_rand();
        bank.transfer(rent_exempt, &mint_keypair, &unstaked.node_keypair.pubkey())
            .unwrap();
        let vote_pubkey = unstaked.vote_keypair.pubkey();
        bank.store_account(
            &vote_pubkey,
            &vote_state::create_v4_account_with_authorized(
                &unstaked.node_keypair.pubkey(),
                &vote_pubkey,
                unstaked.bls_keypair.public.to_bytes_compressed(),
                &vote_pubkey,
                0,
                &vote_pubkey,
                0,
                &vote_pubkey,
                bank.get_minimum_balance_for_rent_exemption(VoteStateV4::size_of()),
            ),
        );

        // A bundle pairing an ordinary transfer with a below-floor vote.
        let build_batch = || {
            sanitize_transactions(vec![
                system_transaction::transfer(
                    &mint_keypair,
                    &solana_pubkey::new_rand(),
                    rent_exempt,
                    bank.confirmed_last_blockhash(),
                ),
                new_tower_sync_transaction(
                    TowerSync::from(vec![(bank.slot().saturating_sub(1), 1)]),
                    bank.confirmed_last_blockhash(),
                    &unstaked.node_keypair,
                    &unstaked.vote_keypair,
                    &unstaked.vote_keypair,
                    None,
                ),
            ])
        };

        // Independent transactions: the floor removes only the vote, and the
        // unrelated transfer still lands.
        let batch = build_batch();
        let ExecuteAndCommitTransactionsOutput {
            transaction_counts, ..
        } = consumer
            .process_and_record_aged_transactions(
                &bank,
                &batch,
                &[MaxAge::MAX, MaxAge::MAX],
                ExecutionFlags::default(),
            )
            .execute_and_commit_transactions_output;
        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 2,
                processed_count: 1,
                processed_with_successful_result_count: 1,
            },
            "the floor must remove the vote without disturbing the rest of the batch"
        );

        // All-or-nothing bundle: the packer asked for atomicity, so a bundle
        // carrying an inadmissible vote must commit nothing at all.
        let batch = build_batch();
        let ExecuteAndCommitTransactionsOutput {
            commit_transactions_result,
            ..
        } = consumer
            .process_and_record_aged_transactions(
                &bank,
                &batch,
                &[MaxAge::MAX, MaxAge::MAX],
                ExecutionFlags {
                    drop_on_failure: true,
                    all_or_nothing: true,
                },
            )
            .execute_and_commit_transactions_output;
        assert!(
            commit_transactions_result
                .as_ref()
                .map(|results| {
                    results
                        .iter()
                        .all(|r| matches!(r, CommitTransactionDetails::NotCommitted(_)))
                })
                .unwrap_or(true),
            "an all-or-nothing bundle containing a below-floor vote must commit nothing, got \
             {commit_transactions_result:?}"
        );
    }

    #[test]
    fn test_bank_process_and_record_transactions() {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            mut record_receiver,
            consumer,
        } = setup_test(true, None);

        let pubkey = solana_pubkey::new_rand();
        let transactions = sanitize_transactions(vec![system_transaction::transfer(
            &mint_keypair,
            &pubkey,
            1,
            bank.confirmed_last_blockhash(),
        )]);

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            commit_transactions_result,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 1,
                processed_count: 1,
                processed_with_successful_result_count: 1,
            }
        );
        assert!(commit_transactions_result.is_ok());

        // When poh is near end of slot, it will be shutdown.
        record_receiver.shutdown();

        let record = record_receiver.drain().next().unwrap();
        assert_eq!(record.bank_id, bank.bank_id());
        assert_eq!(record.transaction_batches.len(), 1);
        let transaction_batch = record.transaction_batches[0].clone();
        assert_eq!(transaction_batch.len(), 1);

        let transactions = sanitize_transactions(vec![system_transaction::transfer(
            &mint_keypair,
            &pubkey,
            2,
            bank.confirmed_last_blockhash(),
        )]);

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            retryable_transaction_indexes,
            commit_transactions_result,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;
        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 1,
                // Transaction was still processed, just wasn't committed, so should be counted here.
                processed_count: 1,
                processed_with_successful_result_count: 1,
            }
        );
        assert_eq!(
            retryable_transaction_indexes,
            vec![RetryableIndex {
                index: 0,
                immediately_retryable: true
            }]
        );
        assert_matches!(
            commit_transactions_result,
            Err(PohRecorderError::MaxHeightReached)
        );

        assert_eq!(bank.get_balance(&pubkey), 1);
    }

    #[test]
    fn test_bank_nonce_update_blockhash_queried_before_transaction_record() {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            record_receiver: _record_receiver,
            consumer,
        } = setup_test(true, None);
        let pubkey = Pubkey::new_unique();

        // setup nonce account with a durable nonce different from the current
        // bank so that it can be advanced in this bank
        let durable_nonce = DurableNonce::from_blockhash(&Hash::new_unique());
        let nonce_hash = *durable_nonce.as_hash();
        let nonce_pubkey = Pubkey::new_unique();
        let nonce_state = nonce::state::State::Initialized(nonce::state::Data {
            authority: mint_keypair.pubkey(),
            durable_nonce,
            fee_calculator: FeeCalculator::new(5000),
        });

        store_nonce_account(&bank, nonce_pubkey, nonce_state);

        // setup a valid nonce tx which will fail during execution
        let transactions = sanitize_transactions(vec![system_transaction::nonced_transfer(
            &mint_keypair,
            &pubkey,
            u64::MAX,
            &nonce_pubkey,
            &mint_keypair,
            nonce_hash,
        )]);
        // get original backhash before we tick to the end.
        let bank_hash = bank.last_blockhash();

        while bank.tick_height() != bank.max_tick_height() - 1 {
            bank.register_default_tick_for_test();
        }

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);
        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            commit_transactions_result,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 1,
                processed_count: 1,
                processed_with_successful_result_count: 0,
            }
        );
        assert!(commit_transactions_result.is_ok());
        bank.register_default_tick_for_test();

        // check that the nonce was advanced to the current bank's last blockhash
        // rather than the current bank's blockhash as would occur had the update
        // blockhash been queried _after_ transaction recording
        let expected_nonce = DurableNonce::from_blockhash(&bank_hash);
        let expected_nonce_hash = expected_nonce.as_hash();
        let nonce_account = bank.get_account(&nonce_pubkey).unwrap();
        assert!(verify_nonce_account(&nonce_account, expected_nonce_hash).is_some());
    }

    #[test]
    fn test_bank_process_and_record_transactions_all_unexecuted() {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            record_receiver: _record_receiver,
            consumer,
        } = setup_test(true, None);

        let pubkey = solana_pubkey::new_rand();
        let transactions = {
            let mut tx =
                system_transaction::transfer(&mint_keypair, &pubkey, 1, bank.last_blockhash());
            // Add duplicate account key
            tx.message.account_keys.push(pubkey);
            sanitize_transactions(vec![tx])
        };

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            commit_transactions_result,
            retryable_transaction_indexes,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 1,
                processed_count: 0,
                processed_with_successful_result_count: 0,
            }
        );
        assert!(retryable_transaction_indexes.is_empty());
        assert_eq!(
            commit_transactions_result.ok(),
            Some(vec![
                CommitTransactionDetails::NotCommitted(
                    TransactionError::AccountLoadedTwice
                );
                1
            ])
        );
    }

    #[test_case(false; "old")]
    #[test_case(true; "simd83")]
    fn test_bank_process_and_record_transactions_cost_tracker(
        relax_intrabatch_account_locks: bool,
    ) {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            record_receiver: _record_receiver,
            consumer,
        } = setup_test(relax_intrabatch_account_locks, None);

        let pubkey = solana_pubkey::new_rand();

        let get_block_cost = || bank.read_cost_tracker().unwrap().block_cost();
        let get_tx_count = || bank.read_cost_tracker().unwrap().transaction_count();
        assert_eq!(get_block_cost(), 0);
        assert_eq!(get_tx_count(), 0);

        //
        // TEST: cost tracker's block cost increases when successfully processing a tx
        //

        let transactions = sanitize_transactions(vec![system_transaction::transfer(
            &mint_keypair,
            &pubkey,
            1,
            bank.last_blockhash(),
        )]);

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            commit_transactions_result,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;
        assert_eq!(transaction_counts.processed_with_successful_result_count, 1);
        assert!(commit_transactions_result.is_ok());

        let block_cost = get_block_cost();
        assert_ne!(block_cost, 0);
        assert_eq!(get_tx_count(), 1);

        // TEST: it's expected that the allocation will execute but the transfer will not
        // because of a shared write-lock between mint_keypair. Ensure only the first transaction
        // takes compute units in the block
        let allocate_keypair = Keypair::new();
        let transactions = sanitize_transactions(vec![
            system_transaction::allocate(
                &mint_keypair,
                &allocate_keypair,
                bank.last_blockhash(),
                100,
            ),
            // this one won't execute in process_and_record_transactions from shared account lock overlap
            system_transaction::transfer(&mint_keypair, &pubkey, 2, bank.last_blockhash()),
        ]);

        let conflicting_transaction = sanitize_transactions(vec![system_transaction::transfer(
            &Keypair::new(),
            &pubkey,
            1,
            bank.last_blockhash(),
        )]);
        bank.try_lock_accounts(&conflicting_transaction);

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            commit_transactions_result,
            retryable_transaction_indexes,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;
        assert_eq!(transaction_counts.processed_with_successful_result_count, 1);
        assert!(commit_transactions_result.is_ok());

        // first one should have been committed, second one not committed due to AccountInUse error during
        // account locking
        let commit_transactions_result = commit_transactions_result.unwrap();
        assert_eq!(commit_transactions_result.len(), 2);
        assert_matches!(
            commit_transactions_result.first(),
            Some(CommitTransactionDetails::Committed { .. })
        );
        assert_matches!(
            commit_transactions_result.get(1),
            Some(CommitTransactionDetails::NotCommitted(_))
        );
        assert_eq!(
            retryable_transaction_indexes,
            vec![RetryableIndex::new(1, true)]
        );

        let expected_block_cost = {
            let (actual_programs_execution_cost, actual_loaded_accounts_data_size_cost) =
                match commit_transactions_result.first().unwrap() {
                    CommitTransactionDetails::Committed {
                        compute_units,
                        loaded_accounts_data_size,
                        result: _,
                        fee_payer_post_balance: _,
                    } => (
                        *compute_units,
                        CostModel::calculate_loaded_accounts_data_size_cost(
                            *loaded_accounts_data_size,
                            &bank.feature_set,
                        ),
                    ),
                    CommitTransactionDetails::NotCommitted(_err) => {
                        unreachable!()
                    }
                };

            let mut cost = CostModel::calculate_cost(&transactions[0], &bank.feature_set);
            if let TransactionCost::Transaction(ref mut usage_cost) = cost {
                usage_cost.programs_execution_cost = actual_programs_execution_cost;
                usage_cost.loaded_accounts_data_size_cost = actual_loaded_accounts_data_size_cost;
            }

            block_cost + cost.sum()
        };

        assert_eq!(get_block_cost(), expected_block_cost);
        assert_eq!(get_tx_count(), 2);
    }

    #[test_case(false, false; "old::locked")]
    #[test_case(false, true; "old::duplicate")]
    #[test_case(true, false; "simd83::locked")]
    #[test_case(true, true; "simd83::duplicate")]
    fn test_bank_process_and_record_transactions_account_in_use(
        relax_intrabatch_account_locks: bool,
        use_duplicate_transaction: bool,
    ) {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            record_receiver: _record_receiver,
            consumer,
        } = setup_test(relax_intrabatch_account_locks, None);

        let pubkey = solana_pubkey::new_rand();
        let pubkey1 = solana_pubkey::new_rand();

        let transactions = sanitize_transactions(vec![
            system_transaction::transfer(&mint_keypair, &pubkey, 1, bank.last_blockhash()),
            system_transaction::transfer(
                &mint_keypair,
                if use_duplicate_transaction {
                    &pubkey
                } else {
                    &pubkey1
                },
                1,
                bank.last_blockhash(),
            ),
        ]);
        assert_eq!(
            transactions[0].message_hash() == transactions[1].message_hash(),
            use_duplicate_transaction
        );

        // with simd83 and no duplicate, we take a cross-batch lock on an account to create a conflict
        // with a duplicate transaction and simd83 it comes from message hash equality in the batch
        // without simd83 the conflict comes from locks in batch
        if relax_intrabatch_account_locks && !use_duplicate_transaction {
            let conflicting_transaction =
                sanitize_transactions(vec![system_transaction::transfer(
                    &Keypair::new(),
                    &pubkey1,
                    1,
                    bank.last_blockhash(),
                )]);
            bank.try_lock_accounts(&conflicting_transaction);
        }

        let process_transactions_batch_output =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ExecuteAndCommitTransactionsOutput {
            transaction_counts,
            retryable_transaction_indexes,
            commit_transactions_result,
            ..
        } = process_transactions_batch_output.execute_and_commit_transactions_output;

        assert_eq!(
            transaction_counts,
            LeaderProcessedTransactionCounts {
                attempted_processing_count: 2,
                processed_count: 1,
                processed_with_successful_result_count: 1,
            }
        );
        assert!(commit_transactions_result.is_ok());

        // with simd3, duplicate transactions are not retryable
        if relax_intrabatch_account_locks && use_duplicate_transaction {
            assert_eq!(retryable_transaction_indexes, Vec::<_>::new());
        } else {
            assert_eq!(
                retryable_transaction_indexes,
                vec![RetryableIndex::new(1, true)]
            );
        }
    }

    #[test]
    fn test_process_transactions_instruction_error() {
        agave_logger::setup();
        let lamports = 10_000;
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_slow_genesis_config(lamports);
        let (bank, _bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        // set cost tracker limits to MAX so it will not filter out TXs
        bank.write_cost_tracker()
            .unwrap()
            .set_limits(u64::MAX, u64::MAX, u64::MAX);

        // Transfer more than the balance of the mint keypair, should cause a
        // InstructionError::InsufficientFunds that is then committed.
        let transactions = vec![system_transaction::transfer(
            &mint_keypair,
            &Pubkey::new_unique(),
            lamports + 1,
            genesis_config.hash(),
        )];

        let transactions_len = transactions.len();
        let ProcessTransactionBatchOutput {
            execute_and_commit_transactions_output,
            ..
        } = execute_transactions_for_test(bank, transactions);

        // All the transactions should have been replayed
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .attempted_processing_count,
            transactions_len as u64
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_count,
            1,
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_with_successful_result_count,
            0,
        );

        assert_eq!(
            execute_and_commit_transactions_output.retryable_transaction_indexes,
            (1..transactions_len - 1)
                .map(|index| RetryableIndex::new(index, true))
                .collect::<Vec<_>>()
        );
    }

    #[test_case(false, false; "old::locked")]
    #[test_case(false, true; "old::duplicate")]
    #[test_case(true, false; "simd83::locked")]
    #[test_case(true, true; "simd83::duplicate")]
    fn test_process_transactions_account_in_use(
        relax_intrabatch_account_locks: bool,
        use_duplicate_transaction: bool,
    ) {
        agave_logger::setup();
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_slow_genesis_config(10_000);
        let mut bank = Bank::new_for_tests(&genesis_config);
        if !relax_intrabatch_account_locks {
            bank.deactivate_feature(&agave_feature_set::relax_intrabatch_account_locks::id());
        }
        bank.ns_per_slot = u128::MAX;
        let (bank, _bank_forks) = bank.wrap_with_bank_forks_for_tests();
        // set cost tracker limits to MAX so it will not filter out TXs
        bank.write_cost_tracker()
            .unwrap()
            .set_limits(u64::MAX, u64::MAX, u64::MAX);

        let mut transactions = vec![];
        let destination = Pubkey::new_unique();
        let mut amount = 1;

        // Make distinct, or identical, transactions that conflict on the `mint_keypair`
        for _ in 0..TARGET_NUM_TRANSACTIONS_PER_BATCH {
            transactions.push(system_transaction::transfer(
                &mint_keypair,
                &destination,
                amount,
                genesis_config.hash(),
            ));

            if !use_duplicate_transaction {
                amount += 1;
            }
        }

        let transactions_len = transactions.len();
        let ProcessTransactionBatchOutput {
            execute_and_commit_transactions_output,
            ..
        } = execute_transactions_for_test(bank, transactions);

        // If SIMD-83 is enabled *and* the transactions are distinct, all are executed.
        // In the three other cases, only one is executed. In all four cases, all are attempted.
        let execution_count = if relax_intrabatch_account_locks && !use_duplicate_transaction {
            transactions_len
        } else {
            1
        } as u64;

        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .attempted_processing_count,
            transactions_len as u64
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_count,
            execution_count
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_with_successful_result_count,
            execution_count
        );

        // If SIMD-83 is enabled and the transactions are distinct, there are zero retryable (all executed).
        // If SIMD-83 is enabled and the transactions are identical, there are zero retryable (marked AlreadyProcessed).
        // If SIMD-83 is not enabled, all but the first are retryable (marked AccountInUse).
        if relax_intrabatch_account_locks {
            assert_eq!(
                execute_and_commit_transactions_output.retryable_transaction_indexes,
                Vec::<_>::new()
            );
        } else {
            assert_eq!(
                execute_and_commit_transactions_output.retryable_transaction_indexes,
                (1..transactions_len)
                    .map(|index| RetryableIndex::new(index, true))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_process_transactions_returns_unprocessed_txs() {
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            mut record_receiver,
            consumer,
        } = setup_test(true, None);

        let pubkey = solana_pubkey::new_rand();

        let transactions = sanitize_transactions(vec![system_transaction::transfer(
            &mint_keypair,
            &pubkey,
            1,
            bank.last_blockhash(),
        )]);

        // Channel shutdown should result in error returned on record.
        record_receiver.shutdown();

        let process_transactions_summary =
            consumer.process_and_record_transactions(&bank, &transactions);

        let ProcessTransactionBatchOutput {
            mut execute_and_commit_transactions_output,
            ..
        } = process_transactions_summary;

        // Transaction is successfully processed, but not committed due to poh recording error.
        assert!(
            execute_and_commit_transactions_output
                .commit_transactions_result
                .is_err()
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .attempted_processing_count,
            1
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_count,
            1
        );
        assert_eq!(
            execute_and_commit_transactions_output
                .transaction_counts
                .processed_with_successful_result_count,
            1
        );

        execute_and_commit_transactions_output
            .retryable_transaction_indexes
            .sort_unstable();
        let expected: Vec<_> = (0..transactions.len())
            .map(|index| RetryableIndex::new(index, true))
            .collect();
        assert_eq!(
            execute_and_commit_transactions_output.retryable_transaction_indexes,
            expected
        );
    }

    #[test]
    fn test_write_persist_transaction_status() {
        let (transaction_status_sender, transaction_status_receiver) = unbounded();
        let tss = Some(TransactionStatusSender {
            sender: transaction_status_sender,
            dependency_tracker: None,
        });
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks: _bank_forks,
            record_receiver: _record_receiver,
            consumer,
        } = setup_test(true, tss);

        let pubkey = solana_pubkey::new_rand();
        let pubkey1 = solana_pubkey::new_rand();
        let keypair1 = Keypair::new();

        let rent_exempt_amount = bank.get_minimum_balance_for_rent_exemption(0);
        assert!(rent_exempt_amount > 0);

        let success_tx = system_transaction::transfer(
            &mint_keypair,
            &pubkey,
            rent_exempt_amount,
            bank.last_blockhash(),
        );
        let ix_error_tx = system_transaction::transfer(
            &keypair1,
            &pubkey1,
            2 * rent_exempt_amount,
            bank.last_blockhash(),
        );

        let transactions = sanitize_transactions(vec![success_tx, ix_error_tx]);
        let batch_transactions_inner = transactions
            .iter()
            .map(|tx| tx.clone().into_inner_transaction())
            .collect::<Vec<_>>();
        bank.transfer(rent_exempt_amount, &mint_keypair, &keypair1.pubkey())
            .unwrap();

        let _ = consumer.process_and_record_transactions(&bank, &transactions);
        drop(consumer); // drop/disconnect transaction_status_sender

        let status_messages = transaction_status_receiver.into_iter().collect::<Vec<_>>();
        assert_eq!(status_messages.len(), 1);
        let TransactionStatusMessage::Batch((status_batch, _)) =
            status_messages.into_iter().next().unwrap()
        else {
            panic!("not a batch");
        };
        assert_eq!(status_batch.transactions, batch_transactions_inner);
        let commit_results = status_batch
            .commit_results
            .into_iter()
            .map(|r| r.unwrap().status)
            .collect::<Vec<_>>();
        assert_eq!(
            commit_results,
            vec![
                Ok(()),
                Err(TransactionError::InstructionError(
                    0,
                    InstructionError::Custom(1)
                ))
            ]
        );
    }

    #[test]
    fn test_write_persist_loaded_addresses() {
        let (transaction_status_sender, transaction_status_receiver) = unbounded();
        let tss = Some(TransactionStatusSender {
            sender: transaction_status_sender,
            dependency_tracker: None,
        });
        let TestFrame {
            mint_keypair,
            bank,
            bank_forks,
            mut record_receiver,
            consumer,
        } = setup_test(true, tss);

        let keypair = Keypair::new();
        let address_table_key = Pubkey::new_unique();
        let address_table_state = generate_new_address_lookup_table(None, 2);
        store_address_lookup_table(&bank, address_table_key, address_table_state);

        let new_bank = Bank::new_from_parent(bank, &Pubkey::new_unique(), 2);
        let bank = bank_forks
            .write()
            .unwrap()
            .insert(new_bank)
            .clone_without_scheduler();

        record_receiver.shutdown();
        record_receiver.restart(bank.bank_id());

        let message = VersionedMessage::V0(v0::Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            recent_blockhash: bank.last_blockhash(),
            account_keys: vec![keypair.pubkey()],
            address_table_lookups: vec![MessageAddressTableLookup {
                account_key: address_table_key,
                writable_indexes: vec![0],
                readonly_indexes: vec![1],
            }],
            instructions: vec![],
        });

        let tx = VersionedTransaction::try_new(message, &[&keypair]).unwrap();
        let sanitized_tx = RuntimeTransaction::try_create(
            tx,
            MessageHash::Compute,
            Some(false),
            bank.as_ref(),
            &ReservedAccountKeys::empty_key_set(),
            bank.feature_set
                .is_active(&agave_feature_set::static_instruction_limit::id()),
            bank.feature_set
                .is_active(&agave_feature_set::limit_instruction_accounts::id()),
        )
        .unwrap();
        let batch_transactions_inner = [&sanitized_tx]
            .into_iter()
            .map(|tx| tx.clone().into_inner_transaction())
            .collect::<Vec<_>>();

        bank.transfer(1, &mint_keypair, &keypair.pubkey()).unwrap();

        let _ = consumer.process_and_record_transactions(&bank, slice::from_ref(&sanitized_tx));
        drop(consumer); // drop/disconnect transaction_status_sender

        let status_messages = transaction_status_receiver.into_iter().collect::<Vec<_>>();
        assert_eq!(status_messages.len(), 1);
        let TransactionStatusMessage::Batch((status_batch, _)) =
            status_messages.into_iter().next().unwrap()
        else {
            panic!("not a batch");
        };
        assert_eq!(status_batch.transactions, batch_transactions_inner);
        assert_eq!(status_batch.commit_results.len(), 1);
        let committed_transaction = status_batch
            .commit_results
            .into_iter()
            .next()
            .unwrap()
            .unwrap();
        assert!(committed_transaction.status.is_ok());
    }
}
