#![cfg_attr(
    not(feature = "agave-unstable-api"),
    deprecated(
        since = "3.1.0",
        note = "This crate has been marked for formal inclusion in the Agave Unstable API. From \
                v4.0.0 onward, the `agave-unstable-api` crate feature must be specified to \
                acknowledge use of an interface that may break without warning."
    )
)]
use {
    agave_feature_set::{
        enable_secp256r1_precompile, validate_fee_vote_transaction_instructions, FeatureSet,
    },
    log::{debug, trace},
    solana_borsh::v1::try_from_slice_unchecked,
    solana_builtins_default_costs::{
        get_builtin_migration_feature_index, BuiltinMigrationFeatureIndex, MAYBE_BUILTIN_KEY,
        MIGRATING_BUILTINS_COSTS,
    },
    solana_compute_budget::compute_budget_limits::{
        DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT, MAX_COMPUTE_UNIT_LIMIT,
    },
    solana_compute_budget_instruction::instructions_processor::process_compute_budget_instructions,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_fee_structure::FeeDetails,
    solana_pubkey::Pubkey,
    solana_sdk_ids::{
        bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, compute_budget, ed25519_program,
        loader_v4, secp256k1_program, stake, system_program, vote,
    },
    solana_svm_transaction::svm_message::SVMMessage,
};

/// Multiplier applied to compute units to calculate base fee.
/// Base Fee = Compute Units × BASE_FEE_MULTIPLIER
pub const BASE_FEE_MULTIPLIER: u64 = 10;

/// Bools indicating the activation of features relevant
/// to the fee calculation.
// DEVELOPER NOTE:
// This struct may become empty at some point. It is preferable to keep it
// instead of removing, since fees will naturally be changed via feature-gates
// in the future. Keeping this struct will help keep things organized.
#[derive(Copy, Clone)]
pub struct FeeFeatures {
    pub enable_secp256r1_precompile: bool,
    pub validate_fee_vote_transaction_instructions: bool,
}

impl From<&FeatureSet> for FeeFeatures {
    fn from(feature_set: &FeatureSet) -> Self {
        Self {
            enable_secp256r1_precompile: feature_set.is_active(&enable_secp256r1_precompile::ID),
            validate_fee_vote_transaction_instructions: feature_set
                .is_active(&validate_fee_vote_transaction_instructions::ID),
        }
    }
}

/// Builtin program compute unit costs.
/// These values match DEFAULT_COMPUTE_UNITS from each program crate.
const BUILTIN_COSTS: &[(&Pubkey, u64)] = &[
    (&vote::ID, 2_100),
    (&stake::ID, 750),
    (&system_program::ID, 150),
    (&compute_budget::ID, 150),
    (&bpf_loader::ID, 570),
    (&bpf_loader_deprecated::ID, 1_140),
    (&bpf_loader_upgradeable::ID, 2_370),
    (&loader_v4::ID, 2_000),
    (&secp256k1_program::ID, 0),
    (&ed25519_program::ID, 0),
];

/// Look up the compute unit cost for a builtin program.
fn lookup_builtin_cost(program_id: &Pubkey) -> Option<u64> {
    BUILTIN_COSTS
        .iter()
        .find(|(id, _)| *id == program_id)
        .map(|(_, cost)| *cost)
}

/// Get the cost for a builtin instruction, or None if not a builtin or has migrated to BPF.
fn get_builtin_instruction_cost(program_id: &Pubkey) -> Option<u64> {
    // Quick filter using the first byte of the pubkey
    if !MAYBE_BUILTIN_KEY[program_id.as_ref()[0] as usize] {
        return None;
    }

    match get_builtin_migration_feature_index(program_id) {
        BuiltinMigrationFeatureIndex::NotBuiltin => None,
        BuiltinMigrationFeatureIndex::BuiltinNoMigrationFeature => lookup_builtin_cost(program_id),
        BuiltinMigrationFeatureIndex::BuiltinWithMigrationFeature(index) => {
            // Assume migration features are active (builtins have migrated to BPF)
            // This is X1's dynamic fees behavior - all features assumed enabled
            let _ = &MIGRATING_BUILTINS_COSTS[index]; // suppress unused warning
            None
        }
    }
}

/// Check if a transaction is a simple vote transaction (vote transactions are fee-exempt).
///
/// When `validate_instructions` is false (legacy behavior), this checks whether the
/// vote program appears in the account keys.
///
/// When `validate_instructions` is true, this mirrors the logic of
/// `is_simple_vote_transaction_impl`: the transaction must have fewer than 3 signatures,
/// no address lookup tables (i.e. legacy message), and exactly one instruction targeting
/// the vote program.
fn is_vote_transaction(message: &impl SVMMessage, validate_instructions: bool) -> bool {
    if !validate_instructions {
        return message.account_keys().iter().any(|key| key == &vote::ID);
    }

    // Mirror is_simple_vote_transaction_impl:
    // 1. has 1 or 2 signatures
    // 2. is legacy message (no address lookup tables)
    // 3. has exactly one instruction which must target the vote program
    if message.num_transaction_signatures() >= 3 {
        return false;
    }
    if message.num_lookup_tables() > 0 {
        return false;
    }
    let mut programs = message.program_instructions_iter().map(|(id, _)| id);
    programs
        .next()
        .xor(programs.next())
        .map(|program_id| program_id == &vote::ID)
        .unwrap_or(false)
}

/// Check if an instruction sets a custom compute unit limit.
fn is_set_compute_unit_limit(program_id: &Pubkey, data: &[u8]) -> bool {
    if compute_budget::check_id(program_id) {
        matches!(
            try_from_slice_unchecked::<ComputeBudgetInstruction>(data),
            Ok(ComputeBudgetInstruction::SetComputeUnitLimit(_))
        )
    } else {
        false
    }
}

/// Calculate the total compute units for a transaction.
fn get_transaction_cost(message: &impl SVMMessage) -> u64 {
    // Use all features enabled for compute budget processing
    let feature_set = FeatureSet::all_enabled();

    let mut builtin_costs = 0u64;
    let mut bpf_costs = 0u64;
    let mut has_custom_compute_limit = false;

    for (program_id, instruction) in message.program_instructions_iter() {
        if let Some(cost) = get_builtin_instruction_cost(program_id) {
            builtin_costs = builtin_costs.saturating_add(cost);
            trace!("Builtin {program_id:?}: {cost} CU (total: {builtin_costs})");
        } else {
            bpf_costs = bpf_costs
                .saturating_add(DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT.into())
                .min(MAX_COMPUTE_UNIT_LIMIT.into());
            trace!("BPF {program_id:?}: assumed {bpf_costs} CU");
        }

        if is_set_compute_unit_limit(program_id, instruction.data) {
            has_custom_compute_limit = true;
        }
    }

    // Override BPF costs if a custom compute unit limit was set
    if bpf_costs > 0 && has_custom_compute_limit {
        if let Ok(limits) =
            process_compute_budget_instructions(message.program_instructions_iter(), &feature_set)
        {
            bpf_costs = u64::from(limits.compute_unit_limit);
            trace!("Custom compute limit: {bpf_costs} CU");
        }
    }

    let total = builtin_costs.saturating_add(bpf_costs);
    trace!("Total cost: {total} (builtin: {builtin_costs}, bpf: {bpf_costs})");
    total
}

/// Calculate fee for a transaction message.
///
/// The fee is calculated as:
/// - Base fee = compute_units × BASE_FEE_MULTIPLIER
/// - Total fee = base_fee + prioritization_fee
///
/// Vote transactions are exempt from fees.
pub fn calculate_fee(
    message: &impl SVMMessage,
    zero_fees_for_test: bool,
    lamports_per_signature: u64,
    prioritization_fee: u64,
    fee_features: FeeFeatures,
) -> u64 {
    calculate_fee_details(
        message,
        zero_fees_for_test,
        lamports_per_signature,
        prioritization_fee,
        fee_features,
    )
    .total_fee()
}

/// Calculate detailed fee breakdown for a transaction message.
///
/// Returns `FeeDetails` containing the base fee and prioritization fee.
/// Vote transactions return zero fees.
pub fn calculate_fee_details(
    message: &impl SVMMessage,
    zero_fees_for_test: bool,
    _lamports_per_signature: u64, // Kept for API compatibility
    prioritization_fee: u64,
    fee_features: FeeFeatures,
) -> FeeDetails {
    if zero_fees_for_test {
        return FeeDetails::default();
    }

    if is_vote_transaction(
        message,
        fee_features.validate_fee_vote_transaction_instructions,
    ) {
        debug!("Vote transaction detected, fee = 0");
        return FeeDetails::default();
    }

    let compute_units = get_transaction_cost(message);
    let base_fee = compute_units.saturating_mul(BASE_FEE_MULTIPLIER);
    let fee_details = FeeDetails::new(base_fee, prioritization_fee);

    debug!(
        "Fee: {} (base: {}, priority: {}, CU: {})",
        fee_details.total_fee(),
        base_fee,
        prioritization_fee,
        compute_units
    );

    fee_details
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        agave_reserved_account_keys::ReservedAccountKeys,
        solana_compute_budget_interface::ComputeBudgetInstruction,
        solana_hash::Hash,
        solana_keypair::Keypair,
        solana_message::{Message, SanitizedMessage},
        solana_native_token::LAMPORTS_PER_SOL,
        solana_signer::Signer,
        solana_system_interface::instruction as system_instruction,
        solana_vote_interface::{instruction as vote_instruction, state::Vote},
        spl_memo_interface::instruction::build_memo,
        test_case::test_case,
    };

    const MICRO_LAMPORTS_PER_LAMPORT: u64 = 1_000_000;

    fn sol_to_lamports(sol: f64) -> u64 {
        (sol * LAMPORTS_PER_SOL as f64) as u64
    }

    fn new_sanitized_message(message: Message) -> SanitizedMessage {
        SanitizedMessage::try_from_legacy_message(message, &ReservedAccountKeys::empty_key_set())
            .unwrap()
    }

    fn get_prioritization_fee(compute_unit_limit: u64, compute_unit_price: u64) -> u64 {
        let micro_lamport_fee =
            (compute_unit_price as u128).saturating_mul(compute_unit_limit as u128);
        micro_lamport_fee
            .saturating_add(MICRO_LAMPORTS_PER_LAMPORT.saturating_sub(1) as u128)
            .checked_div(MICRO_LAMPORTS_PER_LAMPORT as u128)
            .and_then(|fee| u64::try_from(fee).ok())
            .unwrap_or(u64::MAX)
    }

    #[test]
    fn test_calculate_fee_simple_transfer() {
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[system_instruction::transfer(
                &sender.pubkey(),
                &receiver.pubkey(),
                sol_to_lamports(1.0),
            )],
            Some(&sender.pubkey()),
        ));

        // System transfer = 150 CU × 10 = 1500
        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            (&FeatureSet::all_enabled()).into(),
        );
        assert_eq!(fee, 1500);
    }

    #[test_case(300, 1_000_000, 4800; "cu_limit 300, price 1M")]
    #[test_case(300, 10_000_000, 7500; "cu_limit 300, price 10M")]
    #[test_case(0, 0, 4500; "zero limit and price")]
    #[test_case(0, 1, 4500; "zero limit, price 1")]
    #[test_case(999_999, 1, 4501; "limit just under 1 lamport")]
    #[test_case(1_000_000, 1, 4501; "limit exactly 1 lamport")]
    #[test_case(1_000_001, 1, 4502; "limit just over 1 lamport")]
    #[test_case(1_000_000, 1_000_000, 1004500; "1M limit, 1M price")]
    #[test_case(1_000_000, 2_000_000, 2004500; "1M limit, 2M price")]
    #[test_case(u32::MAX, 1, 8795; "max limit, price 1")]
    #[test_case(u32::MAX, u64::MAX, u64::MAX; "max limit and price")]
    #[test_case(u32::MAX, 1_000_000, 4294971795; "max limit, 1M price")]
    #[test_case(1_000_000, u64::MAX, u64::MAX; "1M limit, max price")]
    #[test_case(100_000, 10_000_000, 1004500; "100K limit, 10M price")]
    #[test_case(1_400_000, 1_000_000, 1404500; "1.4M limit, 1M price")]
    #[test_case(1_400_000, u64::MAX, u64::MAX; "1.4M limit, max price")]
    fn test_calculate_fee_with_priority(
        compute_unit_limit: u32,
        compute_unit_price: u64,
        expected: u64,
    ) {
        agave_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
                ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.0),
                ),
            ],
            Some(&sender.pubkey()),
        ));

        let prioritization_fee =
            get_prioritization_fee(compute_unit_limit.into(), compute_unit_price);
        let fee = calculate_fee(
            &message,
            false,
            5000,
            prioritization_fee,
            (&FeatureSet::all_enabled()).into(),
        );
        assert_eq!(fee, expected);
    }

    #[test]
    fn test_calculate_fee_with_bpf_memo() {
        // BPF memo without compute limit uses default 200K CU
        agave_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.0),
                ),
                build_memo(&spl_memo_interface::v3::id(), b"Test memo", &[]),
            ],
            Some(&sender.pubkey()),
        ));

        // System (150) + BPF memo (200,000) = 200,150 × 10 = 2,001,500
        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            (&FeatureSet::all_enabled()).into(),
        );
        assert_eq!(fee, 2001500);
    }

    #[test_case(20_003, 1_000_000, 224533; "20K limit, 1M price")]
    #[test_case(20_003, 10_000_000, 404560; "20K limit, 10M price")]
    #[test_case(100_000, 1_000_000, 1104500; "100K limit, 1M price")]
    #[test_case(400_000, 1_000_000, 4404500; "400K limit, 1M price")]
    #[test_case(400_000, 0, 4004500; "400K limit, zero price")]
    #[test_case(1_400_000, 0, 14004500; "1.4M limit, zero price")]
    #[test_case(1_401_000, 0, 14004500; "1.401M limit, zero price")]
    #[test_case(1_401_000, 1_000_000, 15405500; "1.401M limit, 1M price")]
    fn test_calculate_fee_bpf_memo_with_compute_limits(
        compute_unit_limit: u32,
        compute_unit_price: u64,
        expected: u64,
    ) {
        agave_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
                ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.0),
                ),
                build_memo(&spl_memo_interface::v3::id(), b"Test memo", &[]),
            ],
            Some(&sender.pubkey()),
        ));

        let prioritization_fee =
            get_prioritization_fee(compute_unit_limit.into(), compute_unit_price);
        let fee = calculate_fee(
            &message,
            false,
            5000,
            prioritization_fee,
            (&FeatureSet::all_enabled()).into(),
        );
        assert_eq!(fee, expected);
    }

    /// Helper to build FeeFeatures with the vote validation feature set as specified.
    fn fee_features_with_vote_validation(enabled: bool) -> FeeFeatures {
        FeeFeatures {
            enable_secp256r1_precompile: true,
            validate_fee_vote_transaction_instructions: enabled,
        }
    }

    #[test]
    fn test_real_vote_transaction_is_fee_exempt() {
        // A genuine vote transaction should be fee-exempt regardless of feature gate.
        let vote_account = Keypair::new();
        let authorized_voter = Keypair::new();

        let vote = Vote::new(vec![1, 2, 3], Hash::default());
        let ix = vote_instruction::vote(&vote_account.pubkey(), &authorized_voter.pubkey(), vote);

        let message = new_sanitized_message(Message::new(&[ix], Some(&authorized_voter.pubkey())));

        // Fee-exempt with feature OFF (legacy behavior)
        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(false),
        );
        assert_eq!(fee, 0, "real vote tx should be fee-exempt with feature off");

        // Fee-exempt with feature ON (new behavior)
        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(true),
        );
        assert_eq!(fee, 0, "real vote tx should be fee-exempt with feature on");
    }

    #[test]
    fn test_vote_in_accounts_exempt_without_feature() {
        // Legacy behavior: vote program in account keys triggers fee exemption.
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let mut transfer_ix = system_instruction::transfer(
            &sender.pubkey(),
            &receiver.pubkey(),
            sol_to_lamports(1.0),
        );
        transfer_ix
            .accounts
            .push(solana_instruction::AccountMeta::new_readonly(
                vote::ID,
                false,
            ));

        let message = new_sanitized_message(Message::new(&[transfer_ix], Some(&sender.pubkey())));

        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(false),
        );
        assert_eq!(
            fee, 0,
            "legacy behavior exempts tx with vote program in accounts"
        );
    }

    #[test]
    fn test_vote_in_accounts_charges_fee_with_feature() {
        // With instruction validation enabled, vote program in accounts alone
        // does not qualify for fee exemption.
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let mut transfer_ix = system_instruction::transfer(
            &sender.pubkey(),
            &receiver.pubkey(),
            sol_to_lamports(1.0),
        );
        transfer_ix
            .accounts
            .push(solana_instruction::AccountMeta::new_readonly(
                vote::ID,
                false,
            ));

        let message = new_sanitized_message(Message::new(&[transfer_ix], Some(&sender.pubkey())));

        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(true),
        );
        assert_ne!(
            fee, 0,
            "instruction validation should require actual vote instruction"
        );
    }

    #[test]
    fn test_transfer_with_no_vote_charges_fee() {
        // Basic sanity check: a plain transfer is never fee-exempt.
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[system_instruction::transfer(
                &sender.pubkey(),
                &receiver.pubkey(),
                sol_to_lamports(1.0),
            )],
            Some(&sender.pubkey()),
        ));

        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(true),
        );
        assert_ne!(fee, 0);
    }

    #[test]
    fn test_vote_with_extra_instruction_not_exempt_with_feature() {
        // A transaction with a vote instruction AND a system transfer should NOT be
        // fee-exempt under the new behavior (is_simple_vote requires exactly 1 instruction).
        let vote_account = Keypair::new();
        let authorized_voter = Keypair::new();
        let receiver = Keypair::new();

        let vote = Vote::new(vec![1], Hash::default());
        let vote_ix =
            vote_instruction::vote(&vote_account.pubkey(), &authorized_voter.pubkey(), vote);
        let transfer_ix = system_instruction::transfer(
            &authorized_voter.pubkey(),
            &receiver.pubkey(),
            sol_to_lamports(0.1),
        );

        let message = new_sanitized_message(Message::new(
            &[vote_ix, transfer_ix],
            Some(&authorized_voter.pubkey()),
        ));

        // With feature ON: multi-instruction tx is not a simple vote
        let fee = calculate_fee(
            &message,
            false,
            5000,
            0,
            fee_features_with_vote_validation(true),
        );
        assert_ne!(fee, 0, "vote + transfer should not be fee-exempt");
    }

    #[test]
    fn test_is_vote_transaction_legacy_checks_accounts() {
        let sender = Keypair::new();
        let receiver = Keypair::new();

        // Transfer without vote program in accounts
        let message = new_sanitized_message(Message::new(
            &[system_instruction::transfer(
                &sender.pubkey(),
                &receiver.pubkey(),
                sol_to_lamports(1.0),
            )],
            Some(&sender.pubkey()),
        ));
        assert!(!is_vote_transaction(&message, false));

        // Transfer with vote program in accounts
        let mut ix = system_instruction::transfer(
            &sender.pubkey(),
            &receiver.pubkey(),
            sol_to_lamports(1.0),
        );
        ix.accounts
            .push(solana_instruction::AccountMeta::new_readonly(
                vote::ID,
                false,
            ));
        let message = new_sanitized_message(Message::new(&[ix], Some(&sender.pubkey())));
        assert!(is_vote_transaction(&message, false));
    }

    #[test]
    fn test_is_vote_transaction_validated_checks_instructions() {
        let sender = Keypair::new();
        let receiver = Keypair::new();

        // Non-vote instruction with vote program in accounts — not a vote tx
        let mut ix = system_instruction::transfer(
            &sender.pubkey(),
            &receiver.pubkey(),
            sol_to_lamports(1.0),
        );
        ix.accounts
            .push(solana_instruction::AccountMeta::new_readonly(
                vote::ID,
                false,
            ));
        let message = new_sanitized_message(Message::new(&[ix], Some(&sender.pubkey())));
        assert!(!is_vote_transaction(&message, true));

        // Real vote instruction
        let vote_account = Keypair::new();
        let authorized_voter = Keypair::new();
        let vote = Vote::new(vec![1], Hash::default());
        let vote_ix =
            vote_instruction::vote(&vote_account.pubkey(), &authorized_voter.pubkey(), vote);
        let message =
            new_sanitized_message(Message::new(&[vote_ix], Some(&authorized_voter.pubkey())));
        assert!(is_vote_transaction(&message, true));
    }
}
