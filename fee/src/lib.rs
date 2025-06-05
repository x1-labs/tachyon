use {
    agave_feature_set::{enable_secp256r1_precompile, FeatureSet},
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
    prioritization_fee: u64,
    _fee_features: FeeFeatures,
) -> FeeDetails {
    if zero_fees_for_test {
        return FeeDetails::default();
    }

    if is_vote_transaction(message) {
        trace!("Vote program detected, setting total_fee to 0");
        return FeeDetails::default();
    }

    let derived_compute_units = get_transaction_cost(message);
    let requested_cu_price = get_compute_unit_price_from_message(message);

    trace!(
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

    // Base fee: fixed multiplier + proportional to CU price
    let base_fee = derived_compute_units.saturating_mul(BASE_FEE_MULTIPLIER);
    let price_fee =
        derived_compute_units.saturating_mul(effective_cu_price) / MICROLAMPORTS_PER_LAMPORT;

    let transaction_fee = base_fee.saturating_add(price_fee);
    let fee_details = FeeDetails::new(transaction_fee, prioritization_fee);

    debug!(
        "Calculated transaction_fee: {transaction_fee} | total_fee: {} | compute_units: {derived_compute_units} | requested_cu_price: {requested_cu_price} | prioritization_fee: {prioritization_fee}",
        fee_details.total_fee()
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
    let (mut builtin_costs, mut bpf_costs, mut data_bytes_len_total): (u64, u64, u64) = (0, 0, 0);
    let feature_set = &FeatureSet::all_enabled();

    let compute_unit_limit_is_set =
        message
            .program_instructions_iter()
            .any(|(program_id, instruction)| {
                if let Some(builtin_cost) = get_builtin_instruction_cost(program_id, feature_set) {
                    builtin_costs = builtin_costs.saturating_add(builtin_cost);
                } else {
                    bpf_costs = bpf_costs
                        .saturating_add(solana_compute_budget::compute_budget_limits::DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT.into())
                        .min(solana_compute_budget::compute_budget_limits::MAX_COMPUTE_UNIT_LIMIT.into());
                };

                data_bytes_len_total =
                    data_bytes_len_total.saturating_add(instruction.data.len() as u64);

                check_id(program_id)
                    && try_from_slice_unchecked::<ComputeBudgetInstruction>(instruction.data)
                        .ok()
                        .is_some_and(|i| {
                            matches!(i, ComputeBudgetInstruction::SetComputeUnitLimit(_))
                        })
            });

    if let Ok(compute_budget_limits) =
        process_compute_budget_instructions(message.program_instructions_iter(), feature_set)
    {
        if bpf_costs > 0 && compute_unit_limit_is_set {
            bpf_costs = u64::from(compute_budget_limits.compute_unit_limit);
        }
    }

    builtin_costs.saturating_add(bpf_costs)
}
