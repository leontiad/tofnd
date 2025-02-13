//! Custom error types for [kv_manager].

/// Note: While tofnd generally uses the [anyhow] crate for error handling, we
/// use the [thiserror] crate here for two reasons:
/// 1. [crate::gg20::mnemonic] errors can be potentially consumed by the caller
/// of tofnd, so an analytical display of errors might be helpful in the future.
/// One of the errors that are propagated to [crate::gg20::mnemonic] are
/// [crate::kv_manager::error]s
/// 2. This can be used as an example on how analytical error handling can be
/// incorporated in other modules
/// For more info, see discussion in https://github.com/axelarnetwork/tofnd/issues/28
use crate::encrypted_sled;

#[allow(clippy::enum_variant_names)] // allow Err postfix
#[derive(thiserror::Error, Debug)]
pub enum KvError {
    #[error("Kv initialization Error: {0}")]
    InitErr(#[from] encrypted_sled::Error),
    #[error("Recv Error: {0}")] // errors receiving from "actor pattern"'s channels
    RecvErr(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Send Error: {0}")] // errors sending to "actor pattern"'s channels
    SendErr(String),
    #[error("Reserve Error: {0}")]
    ReserveErr(InnerKvError),
    #[error("Put Error: {0}")]
    PutErr(InnerKvError),
    #[error("Get Error: {0}")]
    GetErr(InnerKvError),
    #[error("Exits Error: {0}")]
    ExistsErr(InnerKvError),
}
pub type KvResult<Success> = Result<Success, KvError>;

#[allow(clippy::enum_variant_names)] // allow Err postfix
#[derive(thiserror::Error, Debug)]
pub enum InnerKvError {
    #[error("Sled Error: {0}")] // Delegate Sled's errors
    SledErr(#[from] encrypted_sled::Error),
    #[error("Logical Error: {0}")] // Logical errors (eg double deletion)
    LogicalErr(String),
    #[error("Serialization Error: failed to serialize value")]
    SerializationErr,
    #[error("Deserialization Error: failed to deserialize kvstore bytes")]
    DeserializationErr,
}
pub(super) type InnerKvResult<Success> = Result<Success, InnerKvError>;
