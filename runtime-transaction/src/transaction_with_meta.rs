use {
    crate::transaction_meta::TransactionMeta,
    solana_loader_v3_interface::get_program_data_address,
    solana_message::AccountKeys,
    solana_pubkey::Pubkey,
    solana_svm_transaction::svm_transaction::SVMTransaction,
    solana_transaction::{sanitized::SanitizedTransaction, versioned::VersionedTransaction},
    std::borrow::Cow,
};

/// Additional readonly locks for loader-v3 ProgramData accounts, including potential CPI programs.
/// Explicit account locks take precedence. Lookup table addresses are not account keys.
pub fn implicit_read_locks(account_keys: AccountKeys<'_>) -> impl Iterator<Item = Pubkey> + '_ {
    account_keys
        .iter()
        .map(get_program_data_address)
        .filter(move |address| !account_keys.iter().any(|key| key == address))
}

pub trait TransactionWithMeta: TransactionMeta + SVMTransaction {
    /// Required to interact with geyser plugins.
    /// This function should not be used except for interacting with geyser.
    /// It may do numerous allocations that negatively impact performance.
    fn as_sanitized_transaction(&self) -> Cow<'_, SanitizedTransaction>;
    /// Required to interact with several legacy interfaces that require
    /// `VersionedTransaction`. This should not be used unless necessary, as it
    /// performs numerous allocations that negatively impact performance.
    fn to_versioned_transaction(&self) -> VersionedTransaction;

    /// Returns the serialized transaction size in bytes.
    /// Runtime metadata is not included.
    fn serialized_size(&self) -> usize;
}
