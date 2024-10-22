//! Fee structures.

use std::collections::{HashMap};
use std::str::FromStr;

use lazy_static::lazy_static;
use crate::native_token::sol_to_lamports;
use log::trace;
use solana_program::borsh1::try_from_slice_unchecked;
use solana_program::instruction::{CompiledInstruction, InstructionError};

#[cfg(not(target_os = "solana"))]
use solana_program::message::SanitizedMessage;
use solana_program::pubkey::Pubkey;
use crate::{compute_budget};
use crate::compute_budget::ComputeBudgetInstruction;
use crate::transaction::{TransactionError};

pub const COMPUTE_UNIT_TO_US_RATIO: u64 = 30;
pub const SIGNATURE_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 24;
pub const SECP256K1_VERIFY_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 223;
pub const ED25519_VERIFY_COST: u64 = COMPUTE_UNIT_TO_US_RATIO * 76;
pub const WRITE_LOCK_UNITS: u64 = COMPUTE_UNIT_TO_US_RATIO * 10;
pub const DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT: u32 = 200_000;
pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
pub const HEAP_LENGTH: usize = 32 * 1024;
const MAX_HEAP_FRAME_BYTES: u32 = 256 * 1024;
pub const MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES: u32 = 64 * 1024 * 1024;

lazy_static! {
    pub static ref BUILT_IN_INSTRUCTION_COSTS: HashMap<Pubkey, u64> = [
        (Pubkey::from_str("Stake11111111111111111111111111111111111111").unwrap(), 750u64),
        (Pubkey::from_str("Config1111111111111111111111111111111111111").unwrap(), 450u64),
        (Pubkey::from_str("Vote111111111111111111111111111111111111111").unwrap(), 2_100u64),
        (Pubkey::from_str("11111111111111111111111111111111").unwrap(), 150u64),
        (Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(), 150u64),
        (Pubkey::from_str("AddressLookupTab1e1111111111111111111111111").unwrap(), 750u64),
        (Pubkey::from_str("BPFLoaderUpgradeab1e11111111111111111111111").unwrap(), 2_370u64),
        (Pubkey::from_str("BPFLoader1111111111111111111111111111111111").unwrap(), 1_140u64),
        (Pubkey::from_str("BPFLoader2111111111111111111111111111111111").unwrap(), 570u64),
        (Pubkey::from_str("LoaderV411111111111111111111111111111111111").unwrap(), 2_000u64),
        // Note: These are precompile, run directly in bank during sanitizing;
        (Pubkey::from_str("KeccakSecp256k11111111111111111111111111111").unwrap(), 0u64),
        (Pubkey::from_str("Ed25519SigVerify111111111111111111111111111").unwrap(), 0u64)
    ]
    .iter()
    .cloned()
    .collect();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeBudgetLimits {
    pub updated_heap_bytes: u32,
    pub compute_unit_limit: u32,
    pub compute_unit_price: u64,
    pub loaded_accounts_bytes: u32,
}

fn sanitize_requested_heap_size(bytes: u32) -> bool {
    (u32::try_from(HEAP_LENGTH).unwrap()..=MAX_HEAP_FRAME_BYTES).contains(&bytes)
        && bytes % 1024 == 0
}

pub fn process_compute_budget_instructions<'a>(
    instructions: impl Iterator<Item = (&'a Pubkey, &'a CompiledInstruction)>,
) -> Result<ComputeBudgetLimits, TransactionError> {
    let mut num_non_compute_budget_instructions: u32 = 0;
    let mut updated_compute_unit_limit = None;
    let mut updated_compute_unit_price = None;
    let mut requested_heap_size = None;
    let mut updated_loaded_accounts_data_size_limit = None;

    for (i, (program_id, instruction)) in instructions.enumerate() {
        if compute_budget::check_id(program_id) {
            let invalid_instruction_data_error = TransactionError::InstructionError(
                i as u8,
                InstructionError::InvalidInstructionData,
            );
            let duplicate_instruction_error = TransactionError::DuplicateInstruction(i as u8);

            match try_from_slice_unchecked(&instruction.data) {
                Ok(ComputeBudgetInstruction::RequestHeapFrame(bytes)) => {
                    if requested_heap_size.is_some() {
                        return Err(duplicate_instruction_error);
                    }
                    if sanitize_requested_heap_size(bytes) {
                        requested_heap_size = Some(bytes);
                    } else {
                        return Err(invalid_instruction_data_error);
                    }
                }
                Ok(ComputeBudgetInstruction::SetComputeUnitLimit(compute_unit_limit)) => {
                    if updated_compute_unit_limit.is_some() {
                        return Err(duplicate_instruction_error);
                    }
                    updated_compute_unit_limit = Some(compute_unit_limit);
                }
                Ok(ComputeBudgetInstruction::SetComputeUnitPrice(micro_lamports)) => {
                    if updated_compute_unit_price.is_some() {
                        return Err(duplicate_instruction_error);
                    }
                    updated_compute_unit_price = Some(micro_lamports);
                }
                Ok(ComputeBudgetInstruction::SetLoadedAccountsDataSizeLimit(bytes)) => {
                    if updated_loaded_accounts_data_size_limit.is_some() {
                        return Err(duplicate_instruction_error);
                    }
                    updated_loaded_accounts_data_size_limit = Some(bytes);
                }
                _ => return Err(invalid_instruction_data_error),
            }
        } else {
            // only include non-request instructions in default max calc
            num_non_compute_budget_instructions =
                num_non_compute_budget_instructions.saturating_add(1);
        }
    }

    // sanitize limits
    let updated_heap_bytes = requested_heap_size
        .unwrap_or(u32::try_from(HEAP_LENGTH).unwrap()) // loader's default heap_size
        .min(MAX_HEAP_FRAME_BYTES);

    let compute_unit_limit = updated_compute_unit_limit
        .unwrap_or_else(|| {
            num_non_compute_budget_instructions
                .saturating_mul(DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT)
        })
        .min(MAX_COMPUTE_UNIT_LIMIT);

    let compute_unit_price = updated_compute_unit_price.unwrap_or(0);

    let loaded_accounts_bytes = updated_loaded_accounts_data_size_limit
        .unwrap_or(MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES)
        .min(MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES);

    Ok(ComputeBudgetLimits {
        updated_heap_bytes,
        compute_unit_limit,
        compute_unit_price,
        loaded_accounts_bytes,
    })
}

fn get_compute_unit_price_from_message(
    message: &SanitizedMessage,
) -> u64 {
    let mut compute_unit_price = 0u64;
    // Iterate through instructions and search for ComputeBudgetInstruction::SetComputeUnitPrice
    for (program_id, instruction) in message.program_instructions_iter() {
        if compute_budget::check_id(program_id) {
            if let Ok(ComputeBudgetInstruction::SetComputeUnitPrice(price)) =
                try_from_slice_unchecked(&instruction.data)
            {
                // Set the compute unit price in tx_cost
                compute_unit_price = price;
            }
        }
    }
    compute_unit_price
}

fn get_transaction_cost(
    message: &SanitizedMessage,
) -> (u64, u64) {
    let mut builtin_costs = 0u64;
    let mut bpf_costs = 0u64;
    let mut compute_unit_limit_is_set = false;

    for (program_id, instruction) in message.program_instructions_iter() {
        // to keep the same behavior, look for builtin first
        if let Some(builtin_cost) = BUILT_IN_INSTRUCTION_COSTS.get(program_id) {
            builtin_costs = builtin_costs.saturating_add(*builtin_cost);
        } else {
            bpf_costs = bpf_costs
                .saturating_add(u64::from(DEFAULT_INSTRUCTION_COMPUTE_UNIT_LIMIT))
                .min(u64::from(MAX_COMPUTE_UNIT_LIMIT));
        }
        if compute_budget::check_id(program_id) {
            if let Ok(ComputeBudgetInstruction::SetComputeUnitLimit(_)) =
                try_from_slice_unchecked(&instruction.data)
            {
                compute_unit_limit_is_set = true;
            }
        }
    }

    // if failed to process compute_budget instructions, the transaction will not be executed
    // by `bank`, therefore it should be considered as no execution cost by cost model.
    match process_compute_budget_instructions(message.program_instructions_iter())
    {
        Ok(compute_budget_limits) => {
            // if tx contained user-space instructions and a more accurate estimate available correct it,
            // where "user-space instructions" must be specifically checked by
            // 'compute_unit_limit_is_set' flag, because compute_budget does not distinguish
            // builtin and bpf instructions when calculating default compute-unit-limit. (see
            // compute_budget.rs test `test_process_mixed_instructions_without_compute_budget`)
            if bpf_costs > 0 && compute_unit_limit_is_set {
                bpf_costs = u64::from(compute_budget_limits.compute_unit_limit);
            }
        }
        Err(_) => {
            builtin_costs = 0;
            bpf_costs = 0;
        }
    }

    (builtin_costs,  bpf_costs)
}

/// A fee and its associated compute unit limit
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct FeeBin {
    /// maximum compute units for which this fee will be charged
    pub limit: u64,
    /// fee in lamports
    pub fee: u64,
}

pub struct FeeBudgetLimits {
    pub loaded_accounts_data_size_limit: usize,
    pub heap_cost: u64,
    pub compute_unit_limit: u64,
    pub prioritization_fee: u64,
}

/// Information used to calculate fees
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FeeStructure {
    /// lamports per signature
    pub lamports_per_signature: u64,
    /// lamports_per_write_lock
    pub lamports_per_write_lock: u64,
    /// Compute unit fee bins
    pub compute_fee_bins: Vec<FeeBin>,
}

pub const ACCOUNT_DATA_COST_PAGE_SIZE: u64 = 32_u64.saturating_mul(1024);

impl FeeStructure {
    pub fn new(
        sol_per_signature: f64,
        sol_per_write_lock: f64,
        compute_fee_bins: Vec<(u64, f64)>,
    ) -> Self {
        trace!("Creating FeeStructure with sol_per_signature: {}", sol_per_signature);
        let compute_fee_bins = compute_fee_bins
            .iter()
            .map(|(limit, sol)| FeeBin {
                limit: *limit,
                fee: sol_to_lamports(*sol),
            })
            .collect::<Vec<_>>();
        FeeStructure {
            lamports_per_signature: sol_to_lamports(sol_per_signature),
            lamports_per_write_lock: sol_to_lamports(sol_per_write_lock),
            compute_fee_bins,
        }
    }

    pub fn get_max_fee(&self, num_signatures: u64, num_write_locks: u64) -> u64 {
        let max_fee = num_signatures
            .saturating_mul(self.lamports_per_signature)
            .saturating_add(num_write_locks.saturating_mul(self.lamports_per_write_lock))
            .saturating_add(
                self.compute_fee_bins
                    .last()
                    .map(|bin| bin.fee)
                    .unwrap_or_default(),
            );
        trace!("Calculated max_fee: {}", max_fee);
        max_fee
    }

    pub fn calculate_memory_usage_cost(
        loaded_accounts_data_size_limit: usize,
        heap_cost: u64,
    ) -> u64 {
        let memory_usage_cost = (loaded_accounts_data_size_limit as u64)
            .saturating_add(ACCOUNT_DATA_COST_PAGE_SIZE.saturating_sub(1))
            .saturating_div(ACCOUNT_DATA_COST_PAGE_SIZE)
            .saturating_mul(heap_cost);
        trace!("Calculated memory_usage_cost: {}", memory_usage_cost);
        memory_usage_cost
    }

    /// Calculate fee for `SanitizedMessage`
    #[cfg(not(target_os = "solana"))]
    pub fn calculate_fee(
        &self,
        message: &SanitizedMessage,
        _lamports_per_signature: u64,
        _budget_limits: &FeeBudgetLimits,
        _include_loaded_account_data_size_in_fee: bool,
    ) -> u64 {
        let (builtins_execution_cost, bpf_execution_cost) = get_transaction_cost(&message);
        let compute_unit_price = get_compute_unit_price_from_message(&message);

        let derived_cu = builtins_execution_cost
            .saturating_add(bpf_execution_cost);

        let adjusted_compute_unit_price = if derived_cu < 1000 && compute_unit_price < 1_000_000 {
            1_000_000
        } else {
            compute_unit_price
        };

        let mut total_fee = derived_cu
            .saturating_mul(10) // ensures multiplication doesn't overflow
            .saturating_add(derived_cu.saturating_mul(adjusted_compute_unit_price)
                .saturating_div(1_000_000)); // change to 1_000_000 to convert to micro lamports

        // If the message contains the vote program, set the total fee to 0
        let contains_vote_program = message.account_keys().iter()
            .any(|key| key == &solana_sdk::vote::program::id());
        if contains_vote_program {
            total_fee = 0;
        }

        trace!("total_fee: {}", total_fee);
        total_fee
    }
}

impl Default for FeeStructure {
    fn default() -> Self {
        Self::new(0.000000005, 0.0, vec![(1_400_000, 0.0)])
    }
}

#[cfg(RUSTC_WITH_SPECIALIZATION)]
impl ::solana_frozen_abi::abi_example::AbiExample for FeeStructure {
    fn example() -> Self {
        FeeStructure::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_memory_usage_cost() {
        let heap_cost = 99;
        const K: usize = 1024;

        // accounts data size are priced in block of 32K, ...

        // ... requesting less than 32K should still be charged as one block
        assert_eq!(
            heap_cost,
            FeeStructure::calculate_memory_usage_cost(31 * K, heap_cost)
        );

        // ... requesting exact 32K should be charged as one block
        assert_eq!(
            heap_cost,
            FeeStructure::calculate_memory_usage_cost(32 * K, heap_cost)
        );

        // ... requesting slightly above 32K should be charged as 2 block
        assert_eq!(
            heap_cost * 2,
            FeeStructure::calculate_memory_usage_cost(33 * K, heap_cost)
        );

        // ... requesting exact 64K should be charged as 2 block
        assert_eq!(
            heap_cost * 2,
            FeeStructure::calculate_memory_usage_cost(64 * K, heap_cost)
        );
    }
}

