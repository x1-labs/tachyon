//! Vote-account stake-admission floor.
//!
//! A vote account must hold a minimum activated stake for its fee-exempt votes
//! to be admitted. This module is the single source of truth for that floor,
//! shared by:
//!   * the leader vote-buffer lane (`solana_core::banking_stage::VoteStorage`),
//!     which applies the floor when buffering votes from the TPU-vote port and
//!     gossip,
//!   * all leader block production, via
//!     `solana_core::banking_stage::Consumer::reject_unstaked_vote` applied in
//!     `process_and_record_transactions_with_pre_results` — the single point
//!     every lane funnels through, so the floor holds regardless of which
//!     ingress a vote used, and
//!   * consensus, via [`reject_underfunded_vote_in_consensus`], once a
//!     `vote_min_stake_*` gate is active.
//!
//! Keeping one implementation here — in `runtime`, below `core` — guarantees the
//! leader-side filters and the consensus rule agree exactly on which votes are
//! underfunded. A disagreement would let an honest leader include a vote its own
//! block verification then rejects, forking the leader off the fleet.

use {
    crate::epoch_stakes::VersionedEpochStakes,
    agave_feature_set::{self as feature_set, FeatureSet},
    solana_cluster_type::ClusterType,
    solana_fee::FeeFeatures,
    solana_sdk_ids::vote,
    solana_svm_transaction::svm_message::SVMMessage,
    solana_transaction_error::TransactionError,
};

/// The original "must be staked" floor: a vote account needs non-zero activated
/// stake (a single lamport) for its votes to be admitted. This is the ungated
/// default on every cluster and matches pre-v3.1 behavior, so a node running
/// this binary does not change vote admission until a `vote_min_stake_*` gate is
/// activated.
pub const MUST_BE_STAKED_LAMPORTS: u64 = 1;

/// The 1 XNT floor, in force on real clusters once `vote_min_stake_1_xnt` is
/// active. Raising the floor from "non-zero stake" to 1 XNT sets a meaningful
/// minimum commitment for a vote account to be admitted.
pub const MIN_VOTE_ACCOUNT_STAKE_LAMPORTS: u64 = 1_000_000_000;

/// Escalated floors (10 XNT, then 100 XNT), each behind its own gate so the bar
/// can be tuned upward if operational experience calls for it. Highest active
/// gate wins.
const VOTE_MIN_STAKE_10_XNT_LAMPORTS: u64 = 10_000_000_000;
const VOTE_MIN_STAKE_100_XNT_LAMPORTS: u64 = 100_000_000_000;

/// Effective minimum vote-account stake for `cluster_type` and the active
/// feature set.
///
/// Every cluster defaults to the "must be staked" rule
/// ([`MUST_BE_STAKED_LAMPORTS`]) so the binary is admission-neutral until a gate
/// is activated; development/local clusters always keep that floor so the small
/// stakes used by local-cluster tests can still vote. Real clusters raise the
/// floor to 1 XNT once `vote_min_stake_1_xnt` is active, then optionally to 10 or
/// 100 XNT (highest active gate wins).
pub fn min_vote_account_stake(cluster_type: ClusterType, features: &FeatureSet) -> u64 {
    match cluster_type {
        ClusterType::Development => MUST_BE_STAKED_LAMPORTS,
        ClusterType::Devnet | ClusterType::Testnet | ClusterType::MainnetBeta => {
            if features.is_active(&feature_set::vote_min_stake_100_xnt::id()) {
                VOTE_MIN_STAKE_100_XNT_LAMPORTS
            } else if features.is_active(&feature_set::vote_min_stake_10_xnt::id()) {
                VOTE_MIN_STAKE_10_XNT_LAMPORTS
            } else if features.is_active(&feature_set::vote_min_stake_1_xnt::id()) {
                MIN_VOTE_ACCOUNT_STAKE_LAMPORTS
            } else {
                MUST_BE_STAKED_LAMPORTS
            }
        }
    }
}

/// Whether any `vote_min_stake_*` gate is active. This is the *consensus*
/// enforcement switch: [`reject_underfunded_vote_in_consensus`] is a no-op until
/// a gate flips, so deploying this binary does not change the bank hash. The hash
/// only changes at the gate's coordinated activation slot — which is also when
/// the leader-side floor rises, so both effects turn on together.
///
/// Because activating a gate now changes consensus, the entire fleet must run a
/// binary that honors this rule *before* any `vote_min_stake_*` gate is
/// activated.
pub fn consensus_floor_enforced(features: &FeatureSet) -> bool {
    features.is_active(&feature_set::vote_min_stake_1_xnt::id())
        || features.is_active(&feature_set::vote_min_stake_10_xnt::id())
        || features.is_active(&feature_set::vote_min_stake_100_xnt::id())
}

/// True if `message` rides the vote fee exemption (fee = 0) yet names a vote
/// account holding less than the current floor. This is the shared predicate the
/// leader-side lanes and consensus all agree on, so a vote admitted by a leader
/// is the same set of votes consensus accepts.
pub fn is_underfunded_vote(
    message: &impl SVMMessage,
    epoch_stakes: &VersionedEpochStakes,
    cluster_type: ClusterType,
    features: &FeatureSet,
) -> bool {
    // Only fee-exempt votes are candidates; a vote paying a normal fee already
    // has a real cost and is left alone.
    if !solana_fee::is_vote_fee_exempt(message, FeeFeatures::from(features)) {
        return false;
    }

    let min_stake = min_vote_account_stake(cluster_type, features);
    let account_keys = message.static_account_keys();
    for (program_id, instruction) in message.program_instructions_iter() {
        if program_id != &vote::ID {
            continue;
        }
        // The vote account is the first account of a simple-vote instruction,
        // mirroring the decoding on the vote-buffer lane
        // (`LatestValidatorVote::new_from_view`). A malformed instruction naming
        // no vote account resolves to zero stake and is treated as underfunded.
        let stake = instruction
            .accounts
            .first()
            .and_then(|index| account_keys.get(usize::from(*index)))
            .map(|vote_pubkey| epoch_stakes.vote_account_stake(vote_pubkey))
            .unwrap_or(0);
        if stake < min_stake {
            return true;
        }
    }
    false
}

/// Consensus rule (gated): reject a fee-exempt vote whose vote account is below
/// the floor, so no such vote can land regardless of which leader produced the
/// block. Injected into the transaction check results, an `Err` makes the vote
/// unprocessable — excluded from an honest leader's block, and fatal (dead slot)
/// to any block that force-includes it on replay.
///
/// Returns `Ok(())` with no effect until a `vote_min_stake_*` gate is active,
/// keeping the gate activation the single coordinated bank-hash change.
///
/// Leaders applying the block-production filters never trip this: they drop
/// underfunded votes at production time, in the vote-buffer lane and again at
/// the shared record chokepoint via `Consumer::reject_unstaked_vote`, which
/// shares [`is_underfunded_vote`] with this rule. It exists so the floor holds
/// uniformly regardless of which leader produced the block — including leaders
/// running a binary without those filters.
pub fn reject_underfunded_vote_in_consensus(
    message: &impl SVMMessage,
    epoch_stakes: &VersionedEpochStakes,
    cluster_type: ClusterType,
    features: &FeatureSet,
) -> Result<(), TransactionError> {
    if !consensus_floor_enforced(features) {
        return Ok(());
    }
    if is_underfunded_vote(message, epoch_stakes, cluster_type, features) {
        return Err(TransactionError::InvalidProgramForExecution);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_vote_account_stake_by_cluster_type() {
        // With no gates active every cluster only requires non-zero stake, so the
        // binary is admission-neutral (matches pre-v3.1 behavior).
        let no_gates = FeatureSet::default();
        for cluster_type in [
            ClusterType::Development,
            ClusterType::Devnet,
            ClusterType::Testnet,
            ClusterType::MainnetBeta,
        ] {
            assert_eq!(
                min_vote_account_stake(cluster_type, &no_gates),
                MUST_BE_STAKED_LAMPORTS,
                "{cluster_type:?} must default to the non-zero-stake floor"
            );
        }

        // vote_min_stake_1_xnt raises the floor to 1 XNT on real clusters only;
        // development/local clusters keep the non-zero floor so small-stake test
        // validators can still vote.
        let mut one_xnt = FeatureSet::default();
        one_xnt.activate(&feature_set::vote_min_stake_1_xnt::id(), 0);
        assert_eq!(
            min_vote_account_stake(ClusterType::Development, &one_xnt),
            MUST_BE_STAKED_LAMPORTS
        );
        for cluster_type in [
            ClusterType::Devnet,
            ClusterType::Testnet,
            ClusterType::MainnetBeta,
        ] {
            assert_eq!(
                min_vote_account_stake(cluster_type, &one_xnt),
                MIN_VOTE_ACCOUNT_STAKE_LAMPORTS,
                "{cluster_type:?} must enforce the 1 XNT floor when gated"
            );
        }
    }

    #[test]
    fn test_vote_min_stake_escalation_gates() {
        // The gates raise the real-cluster floor in steps: 1 XNT, then 10, then
        // 100 (highest active gate wins). They never affect development clusters.
        let mut features = FeatureSet::default();
        assert_eq!(
            min_vote_account_stake(ClusterType::MainnetBeta, &features),
            MUST_BE_STAKED_LAMPORTS,
            "no gates active means only non-zero stake is required"
        );

        features.activate(&feature_set::vote_min_stake_1_xnt::id(), 0);
        assert_eq!(
            min_vote_account_stake(ClusterType::MainnetBeta, &features),
            MIN_VOTE_ACCOUNT_STAKE_LAMPORTS
        );

        features.activate(&feature_set::vote_min_stake_10_xnt::id(), 0);
        assert_eq!(
            min_vote_account_stake(ClusterType::MainnetBeta, &features),
            VOTE_MIN_STAKE_10_XNT_LAMPORTS
        );

        features.activate(&feature_set::vote_min_stake_100_xnt::id(), 0);
        assert_eq!(
            min_vote_account_stake(ClusterType::MainnetBeta, &features),
            VOTE_MIN_STAKE_100_XNT_LAMPORTS,
            "100 XNT gate takes precedence over the lower gates"
        );

        // Escalation gates never raise the floor on development clusters.
        assert_eq!(
            min_vote_account_stake(ClusterType::Development, &features),
            MUST_BE_STAKED_LAMPORTS
        );
    }

    #[test]
    fn test_consensus_floor_enforced_switch() {
        // No gate active -> consensus enforcement is OFF, so deploying the binary
        // is bank-hash neutral.
        assert!(!consensus_floor_enforced(&FeatureSet::default()));

        // Any single vote_min_stake gate flips the consensus switch on.
        for gate in [
            feature_set::vote_min_stake_1_xnt::id(),
            feature_set::vote_min_stake_10_xnt::id(),
            feature_set::vote_min_stake_100_xnt::id(),
        ] {
            let mut features = FeatureSet::default();
            features.activate(&gate, 0);
            assert!(
                consensus_floor_enforced(&features),
                "gate {gate} must enable consensus enforcement"
            );
        }
    }
}
