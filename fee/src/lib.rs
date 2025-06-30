use {
    agave_feature_set::FeatureSet,
    log::{debug, trace},
    solana_builtins_default_costs::get_builtin_instruction_cost,
    solana_compute_budget_instruction::instructions_processor::process_compute_budget_instructions,
    solana_fee_structure::FeeDetails,
    solana_sdk::{
        borsh1::try_from_slice_unchecked,
        compute_budget::{check_id, ComputeBudgetInstruction},
    },
    solana_svm_transaction::svm_message::SVMMessage,
};

pub const DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT: u32 = 200_000;
pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
pub const HEAP_LENGTH: usize = 32 * 1024;
pub const MIN_COMPUTE_UNITS_THRESHOLD: u64 = 1_000;
pub const MIN_COMPUTE_UNIT_PRICE_MICROLAMPORTS: u64 = 1_000_000;
pub const BASE_FEE_MULTIPLIER: u64 = 10;
pub const MICROLAMPORTS_PER_LAMPORT: u64 = 1_000_000;

/// Calculate fee for `SanitizedMessage`
pub fn calculate_fee(
    message: &impl SVMMessage,
    zero_fees_for_test: bool,
    lamports_per_signature: u64,
    prioritization_fee: u64,
    feature_set: &FeatureSet,
) -> u64 {
    calculate_fee_details(
        message,
        zero_fees_for_test,
        lamports_per_signature,
        prioritization_fee,
        feature_set,
    )
    .total_fee()
}

pub fn calculate_fee_details(
    message: &impl SVMMessage,
    zero_fees_for_test: bool,
    _lamports_per_signature: u64,
    prioritization_fee: u64,
    feature_set: &FeatureSet,
) -> FeeDetails {
    if zero_fees_for_test {
        return FeeDetails::default();
    }

    if is_vote_transaction(message) {
        debug!("Vote program detected, setting total_fee to 0");
        return FeeDetails::default();
    }

    trace!("Request fee calculation for message: {:?}", message,);

    let compute_units_derived = get_transaction_cost(message, feature_set);

    // Base Fee = Compute Units Derived × 10
    let base_fee = compute_units_derived.saturating_mul(BASE_FEE_MULTIPLIER);
    let fee_details = FeeDetails::new(base_fee, prioritization_fee);

    debug!(
        "Calculated the transaction fee: compute_units_derived: {}, base_fee: {}, prioritization_fee: {}, total_fee: {}",
        compute_units_derived,
        base_fee,
        fee_details.prioritization_fee(),
        fee_details.total_fee(),
    );

    fee_details
}

fn is_vote_transaction(message: &impl SVMMessage) -> bool {
    let vote_program_id = &solana_sdk_ids::vote::ID;
    message
        .account_keys()
        .iter()
        .any(|key| key == vote_program_id)
}

fn get_transaction_cost(message: &impl SVMMessage, feature_set: &FeatureSet) -> u64 {
    let (mut builtin_costs, mut bpf_costs): (u64, u64) = (0, 0);
    let mut compute_unit_limit_is_set = false;

    for (program_id, instruction) in message.program_instructions_iter() {
        if let Some(builtin_cost) = get_builtin_instruction_cost(program_id, feature_set) {
            builtin_costs = builtin_costs.saturating_add(builtin_cost);
            debug!(
                "Added builtin cost for program {:?}: {}, total builtin_costs: {}",
                program_id, builtin_cost, builtin_costs
            );
        } else {
            bpf_costs = bpf_costs
                .saturating_add(solana_compute_budget::compute_budget_limits::DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT.into())
                .min(solana_compute_budget::compute_budget_limits::MAX_COMPUTE_UNIT_LIMIT.into());
            debug!(
                "Assumed BPF instruction for program {:?}, total bpf_costs so far: {}",
                program_id, bpf_costs
            );
        };

        if check_id(program_id) {
            if let Ok(ComputeBudgetInstruction::SetComputeUnitLimit(_)) =
                try_from_slice_unchecked(&instruction.data)
            {
                compute_unit_limit_is_set = true;
                debug!("Found SetComputeUnitLimit instruction");
            }
        }
    }
    debug!(
        "After iterating instructions: builtin_costs: {}, bpf_costs: {}, compute_unit_limit_is_set: {}",
        builtin_costs, bpf_costs, compute_unit_limit_is_set
    );

    if let Ok(compute_budget_limits) =
        process_compute_budget_instructions(message.program_instructions_iter(), feature_set)
    {
        debug!(
            "Processed compute_budget_instructions, got compute_unit_limit: {}",
            compute_budget_limits.compute_unit_limit
        );
        if bpf_costs > 0 && compute_unit_limit_is_set {
            bpf_costs = u64::from(compute_budget_limits.compute_unit_limit);
            debug!(
                "Overriding bpf_costs using SetComputeUnitLimit: {}",
                bpf_costs
            );
        }
    } else {
        debug!("Failed to process compute_budget_instructions");
    }

    debug!(
        "Final builtin_costs: {}, Final bpf_costs: {}",
        builtin_costs, bpf_costs
    );
    builtin_costs.saturating_add(bpf_costs)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_message::Message,
        solana_sdk::{
            message::SanitizedMessage, native_token::sol_to_lamports,
            reserved_account_keys::ReservedAccountKeys, signature::Keypair, signer::Signer,
            system_instruction,
        },
        spl_memo::build_memo,
        test_case::test_case,
    };

    type MicroLamports = u128;
    const MICRO_LAMPORTS_PER_LAMPORT: u64 = 1_000_000;

    pub fn get_prioritization_fee(compute_unit_price: u64, compute_unit_limit: u64) -> u64 {
        debug!(
            "get_prioritization_fee compute_unit_price: {}, compute_unit_limit: {}",
            compute_unit_price, compute_unit_limit
        );
        let micro_lamport_fee: MicroLamports =
            (compute_unit_price as u128).saturating_mul(compute_unit_limit as u128);
        micro_lamport_fee
            .saturating_add(MICRO_LAMPORTS_PER_LAMPORT.saturating_sub(1) as u128)
            .checked_div(MICRO_LAMPORTS_PER_LAMPORT as u128)
            .and_then(|fee| u64::try_from(fee).ok())
            .unwrap_or(u64::MAX)
    }

    fn new_sanitized_message(message: Message) -> SanitizedMessage {
        SanitizedMessage::try_from_legacy_message(message, &ReservedAccountKeys::empty_key_set())
            .unwrap()
    }

    #[test]
    fn test_calculate_fee_simple_transfer() {
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[system_instruction::transfer(
                &sender.pubkey(),
                &receiver.pubkey(),
                sol_to_lamports(1.),
            )],
            Some(&sender.pubkey()),
        ));

        let fee = calculate_fee(&message, false, 5000, 0, &FeatureSet::all_enabled());
        assert_eq!(fee, 1500);
    }

    #[test_case(300, 1_000_000, 4800; "Test with compute unit limit 300 and price 1_000_000")]
    #[test_case(300, 10_000_000, 7500; "Test with compute unit limit 300 and price 10_000_000")]
    #[test_case(0, 0, 4500; "Zero compute_unit_limit and price, only base fee")]
    #[test_case(0, 1, 4500; "Zero compute_unit_limit, price 1")]
    #[test_case(999_999, 1, 4501; "compute_unit_limit just under 1 lamport, price 1")]
    #[test_case(1_000_000, 1, 4501; "compute_unit_limit exactly 1 lamport, price 1")]
    #[test_case(1_000_001, 1, 4502; "compute_unit_limit just over 1 lamport, price 1")]
    #[test_case(1_000_000, 1_000_000, 1004500; "compute_unit_limit 1_000_000, price 1_000_000")]
    #[test_case(1_000_000, 2_000_000, 2004500; "compute_unit_limit 1_000_000, price 2_000_000")]
    #[test_case(u32::MAX, 1, 8795; "Max compute_unit_limit, price 1")]
    #[test_case(u32::MAX, u64::MAX, u64::MAX; "Both limits max, saturating to u64::MAX")]
    #[test_case(u32::MAX, 1_000_000, 4294971795; "Max compute_unit_limit, price 1_000_000")]
    #[test_case(1_000_000, u64::MAX, u64::MAX; "compute_unit_limit 1_000_000, max price, saturate")]
    #[test_case(100_000, 10_000_000, 1004500; "compute_unit_limit 100_000, price 10_000_000")]
    #[test_case(1_400_000, 1_000_000, 1404500; "Max allowed compute_unit_limit, price 1_000_000")]
    #[test_case(1_400_000, u64::MAX, u64::MAX; "Max allowed compute_unit_limit, max price, saturate")]
    fn test_calculate_fee_simple_transfer_with_priority_fee(
        compute_unit_limit: u32,
        compute_unit_price: u64,
        actual: u64,
    ) {
        solana_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
                ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.),
                ),
            ],
            Some(&sender.pubkey()),
        ));

        let prioritization_fee =
            get_prioritization_fee(u64::from(compute_unit_limit), compute_unit_price);
        let fee = calculate_fee(
            &message,
            false,
            5000,
            prioritization_fee,
            &FeatureSet::all_enabled(),
        );
        assert_eq!(fee, actual);
    }

    // will be expensive since we are not providing a compute unit limit
    // bpf instructions will be assumed to use the default compute unit limit of 200_000
    #[test]
    fn test_calculate_fee_with_bpf_memo_instruction() {
        solana_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let memo_instruction = build_memo(b"Test memo", &[]);
        let instructions = vec![
            system_instruction::transfer(&sender.pubkey(), &receiver.pubkey(), sol_to_lamports(1.)),
            memo_instruction,
        ];

        let message = new_sanitized_message(Message::new(&instructions, Some(&sender.pubkey())));

        let fee = calculate_fee(&message, false, 5000, 0, &FeatureSet::all_enabled());
        assert_eq!(fee, 2001500);
    }

    #[test_case(20_003, 1_000_000, 224533; "Test with compute unit limit 20_003 and price 1_000_000")]
    #[test_case(20_003, 10_000_000, 404560; "Test with compute unit limit 20_003 and price 10_000_000")]
    #[test_case(100_000, 1_000_000, 1104500; "Test with compute unit limit 100_000 and price 1_000_000")]
    #[test_case(400_000, 1_000_000, 4404500; "Test with compute unit limit 400_000 and price 1_000_000")]
    #[test_case(400_000, 0, 4004500; "Test with compute unit limit 400_000 and price 0")]
    #[test_case(1_400_000, 0, 14004500; "Test with compute unit limit 1_400_000 and price 0")]
    #[test_case(1_401_000, 0, 14004500; "Test with compute unit limit 1_401_000 and price 0")]
    #[test_case(1_401_000, 1_000_000, 15405500; "Test with compute unit limit 1_401_000 and price 1_000_000")]
    fn test_calculate_fee_with_bpf_memo_instruction_with_compute_limits(
        compute_unit_limit: u32,
        compute_unit_price: u64,
        actual: u64,
    ) {
        solana_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let instructions = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
            ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
            system_instruction::transfer(&sender.pubkey(), &receiver.pubkey(), sol_to_lamports(1.)),
            build_memo(b"Test memo", &[]),
        ];

        let message = new_sanitized_message(Message::new(&instructions, Some(&sender.pubkey())));

        let prioritization_fee =
            get_prioritization_fee(u64::from(compute_unit_limit), compute_unit_price);
        let fee = calculate_fee(
            &message,
            false,
            5000,
            prioritization_fee,
            &FeatureSet::all_enabled(),
        );
        assert_eq!(fee, actual);
    }
}
