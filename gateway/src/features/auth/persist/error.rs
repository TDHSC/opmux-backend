//! Sanitized persistence errors for the authentication store.

/// Failures from parameterized auth-store operations.
///
/// Variants name the business outcome. They never include SQLSTATE detail,
/// constraint text, connection strings, or digest values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthStoreError {
    /// The database is unreachable, timed out, or closed.
    #[error("auth store unavailable")]
    Unavailable,
    /// A client primary key already exists.
    #[error("duplicate client")]
    DuplicateClient,
    /// The SHA-256 digest is already stored.
    #[error("duplicate key digest")]
    DuplicateDigest,
    /// The safe display identifier is already stored.
    #[error("duplicate display id")]
    DuplicateDisplayId,
    /// The referenced client does not exist.
    #[error("unknown client")]
    UnknownClient,
    /// A check or identity-immutability constraint rejected the write.
    #[error("check constraint rejected")]
    CheckViolation,
    /// The connected role lacks the required grant.
    #[error("permission denied")]
    PermissionDenied,
    /// A list limit is zero, negative, or otherwise unusable.
    #[error("invalid list limit")]
    InvalidLimit,
    /// A digest value is not exactly 32 bytes.
    #[error("invalid digest length")]
    InvalidDigestLength,
    /// Another uniqueness conflict on a persisted identity column.
    #[error("conflicting persisted identity")]
    Conflict,
}

impl AuthStoreError {
    /// Maps a SQLx error without retaining its message.
    pub fn from_sqlx(err: sqlx::Error) -> Self {
        if let Some(db) = err.as_database_error() {
            match db.code().as_deref() {
                Some("23505") => match db.constraint() {
                    Some("clients_pkey") => Self::DuplicateClient,
                    Some("api_keys_key_digest_key") => Self::DuplicateDigest,
                    Some("api_keys_display_id_key") => Self::DuplicateDisplayId,
                    _ => Self::Conflict,
                },
                Some("23503") => Self::UnknownClient,
                Some("23514") | Some("23000") | Some("P0001") => Self::CheckViolation,
                Some("42501") => Self::PermissionDenied,
                _ => Self::Unavailable,
            }
        } else {
            match err {
                sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::Io(_)
                | sqlx::Error::Tls(_)
                | sqlx::Error::Protocol(_)
                | sqlx::Error::Configuration(_) => Self::Unavailable,
                _ => Self::Unavailable,
            }
        }
    }
}
