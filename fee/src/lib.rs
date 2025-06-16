use {
    agave_feature_set::{enable_secp256r1_precompile, FeatureSet},
    log::debug,
    solana_builtins_default_costs::get_builtin_instruction_cost,
    solana_compute_budget_instruction::instructions_processor::process_compute_budget_instructions,
    solana_fee_structure::FeeDetails,
    solana_sdk::{
        borsh1::try_from_slice_unchecked,
        compute_budget::{check_id, ComputeBudgetInstruction},
    },
    solana_svm_transaction::svm_message::SVMMessage,
};

/// Bools indicating the activation of features relevant
/// to the fee calculation.
// DEVELOPER NOTE:
// This struct may become empty at some point. It is preferable to keep it
// instead of removing, since fees will naturally be changed via feature-gates
// in the future. Keeping this struct will help keep things organized.
#[derive(Copy, Clone)]
pub struct FeeFeatures {
    pub enable_secp256r1_precompile: bool,
}

pub const DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT: u32 = 200_000;
pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
pub const HEAP_LENGTH: usize = 32 * 1024;
pub const MIN_COMPUTE_UNITS_THRESHOLD: u64 = 1_000;
pub const MIN_COMPUTE_UNIT_PRICE_MICROLAMPORTS: u64 = 1_000_000;
pub const BASE_FEE_MULTIPLIER: u64 = 10;
pub const MICROLAMPORTS_PER_LAMPORT: u64 = 1_000_000;

impl From<&FeatureSet> for FeeFeatures {
    fn from(feature_set: &FeatureSet) -> Self {
        Self {
            enable_secp256r1_precompile: feature_set.is_active(&enable_secp256r1_precompile::ID),
        }
    }
}

/// Calculate fee for `SanitizedMessage`
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

pub fn calculate_fee_details(
    message: &impl SVMMessage,
    zero_fees_for_test: bool,
    _lamports_per_signature: u64,
    _prioritization_fee: u64,
    _fee_features: FeeFeatures,
) -> FeeDetails {
    if zero_fees_for_test {
        return FeeDetails::default();
    }

    if is_vote_transaction(message) {
        debug!("Vote program detected, setting total_fee to 0");
        return FeeDetails::default();
    }

    let derived_compute_units = get_transaction_cost(message);
    let requested_cu_price = get_compute_unit_price_from_message(message);

    debug!(
        "message: {:?}, derived_compute_units: {}, requested_cu_price: {}",
        message,
        derived_compute_units,
        requested_cu_price
    );

    // Ensure minimum price when both CU and price are low
    let effective_cu_price = if derived_compute_units < MIN_COMPUTE_UNITS_THRESHOLD
        && requested_cu_price < MIN_COMPUTE_UNIT_PRICE_MICROLAMPORTS
    {
        MIN_COMPUTE_UNIT_PRICE_MICROLAMPORTS
    } else {
        requested_cu_price
    };
    debug!("effective_cu_price: {}", effective_cu_price);

    // Base fee: fixed multiplier + proportional to CU price
    let base_fee = derived_compute_units.saturating_mul(BASE_FEE_MULTIPLIER);
    let price_fee =
        derived_compute_units.saturating_mul(effective_cu_price) / MICROLAMPORTS_PER_LAMPORT;

    let transaction_fee = base_fee.saturating_add(price_fee);
    let fee_details = FeeDetails::new(transaction_fee, price_fee.saturating_sub(base_fee));

    debug!(
        "Calculated transaction_fee: {transaction_fee} | total_fee: {} | compute_units: {derived_compute_units} | requested_cu_price: {requested_cu_price} | prioritization_fee: {} | price_fee: {}",
        fee_details.total_fee(),
        fee_details.prioritization_fee(),
        price_fee
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

fn get_compute_unit_price_from_message(message: &impl SVMMessage) -> u64 {
    for (program_id, instruction) in message.program_instructions_iter() {
        if check_id(program_id) {
            if let Ok(ComputeBudgetInstruction::SetComputeUnitPrice(price)) =
                try_from_slice_unchecked(instruction.data)
            {
                return price;
            }
        }
    }

    0
}

fn get_transaction_cost(message: &impl SVMMessage) -> u64 {
    let (mut builtin_costs, mut bpf_costs): (u64, u64) = (0, 0);
    let feature_set = &FeatureSet::all_enabled();
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
                try_from_slice_unchecked(&instruction.data) {
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
        process_compute_budget_instructions(message.program_instructions_iter(), feature_set) {
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

    debug!("Final builtin_costs: {}, Final bpf_costs: {}", builtin_costs, bpf_costs);
    builtin_costs.saturating_add(bpf_costs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_message::Message;
    use solana_sdk::native_token::sol_to_lamports;
    use solana_sdk::reserved_account_keys::ReservedAccountKeys;
    use solana_sdk::system_instruction;
    use solana_sdk::message::SanitizedMessage;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;

    fn new_sanitized_message(message: Message) -> SanitizedMessage {
        SanitizedMessage::try_from_legacy_message(message, &ReservedAccountKeys::empty_key_set())
            .unwrap()
    }

    #[test]
    fn test_calculate_fee_simple_transfer() {
        let sender = Keypair::new();
        let receiver = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.),
                )
            ],
            Some(&sender.pubkey()),
        ));

        let fee = calculate_fee(&message, false, 5000, 0, FeeFeatures{ enable_secp256r1_precompile: true });
        assert_eq!(fee, 1650);
    }

    #[test]
    fn test_calculate_fee_simple_transfer_with_set_compute_unit() {
        solana_logger::setup();
        let sender = Keypair::new();
        let receiver = Keypair::new();
        let receiver2 = Keypair::new();

        let message = new_sanitized_message(Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(
                    400_000,
                ),
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver.pubkey(),
                    sol_to_lamports(1.),
                ),
                system_instruction::transfer(
                    &sender.pubkey(),
                    &receiver2.pubkey(),
                    sol_to_lamports(1.),
                ),
            ],
            Some(&sender.pubkey()),
        ));


        let fee = calculate_fee(&message, false, 5000, 0, FeeFeatures{ enable_secp256r1_precompile: true });
        assert_eq!(fee, 4950);
    }

    // #[test]
    // fn test_calculate_fee_basic() {
    //     let msg = DummyMessage;
    //     let lamports_per_signature = 5000;
    //     let prioritization_fee = 0;
    //     let fee = calculate_fee(&msg, false, lamports_per_signature, prioritization_fee, FeeFeatures{ enable_secp256r1_precompile: true });
    //     // 2 signatures * 5000 lamports
    //     assert_eq!(fee, 10000);
    // }
    //
    // #[test]
    // fn test_calculate_fee_with_prioritization() {
    //     let msg = DummyMessage;
    //     let lamports_per_signature = 5000;
    //     let prioritization_fee = 200;
    //     let fee = calculate_fee(&msg, false, lamports_per_signature, prioritization_fee, FeeFeatures{ enable_secp256r1_precompile: true });
    //     // 2 signatures * 5000 lamports + 200 prioritization
    //     assert_eq!(fee, 10200);
    // }
}
