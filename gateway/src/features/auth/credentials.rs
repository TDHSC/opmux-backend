//! Shared API-key generation and hashing.
//!
//! CLI provisioning and later HTTP issuance must call these functions so there
//! is only one credential format and one digest algorithm.

use super::persist::{KeyDigest, KEY_DIGEST_LEN};
use crate::core::config::SecretString;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::fmt;

/// Versioned credential prefix. The random payload follows this token.
pub const CREDENTIAL_PREFIX: &str = "opmx_v1_";
/// CSPRNG payload size in bytes.
pub const SECRET_PAYLOAD_LEN: usize = 32;
/// Random bytes mixed into the safe display identifier.
pub const DISPLAY_ID_RANDOM_LEN: usize = 16;
/// Name assigned to the management key created with a new tenant.
pub const INITIAL_MANAGEMENT_KEY_NAME: &str = "initial-management";

/// Failure from the entropy source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EntropyError {
    /// The CSPRNG could not fill the requested buffer.
    #[error("entropy source failed")]
    Unavailable,
}

/// Cryptographically secure byte source used to generate credentials.
pub trait SecretSource: Send + Sync {
    /// Fills `dest` with random bytes.
    ///
    /// # Parameters
    /// - `dest` - Buffer to overwrite
    ///
    /// # Errors
    /// Returns `Unavailable` when the operating-system CSPRNG fails.
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), EntropyError>;
}

/// Operating-system CSPRNG (`getrandom`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEntropy;

impl SecretSource for SystemEntropy {
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), EntropyError> {
        getrandom::fill(dest).map_err(|_| EntropyError::Unavailable)
    }
}

/// Newly generated secret plus the digest that may be stored.
///
/// The plaintext lives only in this value. `Debug` never prints it.
pub struct GeneratedCredential {
    credential: SecretString,
    digest: KeyDigest,
    display_id: String,
}

impl GeneratedCredential {
    /// Returns the SHA-256 digest of the versioned credential.
    pub fn digest(&self) -> KeyDigest {
        self.digest
    }

    /// Returns the safe display identifier. This is not the secret.
    pub fn display_id(&self) -> &str {
        &self.display_id
    }

    /// Returns the one-time secret for intentional issuance output.
    pub fn expose_secret(&self) -> &str {
        self.credential.expose()
    }

    /// Moves the secret out for a public issuance DTO.
    pub fn into_secret(self) -> SecretString {
        self.credential
    }
}

impl fmt::Debug for GeneratedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedCredential")
            .field("credential", &"[redacted]")
            .field("digest", &self.digest)
            .field("display_id", &self.display_id)
            .finish()
    }
}

/// Hashes a presented credential with SHA-256.
///
/// Call this before any repository access. The digest is over the exact
/// versioned secret string, including `opmx_v1_`.
///
/// # Parameters
/// - `credential` - Presented API key
///
/// # Returns
/// Fixed-length digest suitable for `opmux_private.api_keys.key_digest`
pub fn hash_credential(credential: &str) -> KeyDigest {
    let digest = Sha256::digest(credential.as_bytes());
    let mut bytes = [0_u8; KEY_DIGEST_LEN];
    bytes.copy_from_slice(&digest);
    KeyDigest::from_bytes(bytes)
}

/// Decodes the 32-byte payload from a versioned credential.
///
/// # Parameters
/// - `credential` - Issued `opmx_v1_` secret
///
/// # Returns
/// The raw payload when the prefix and unpadded base64url encoding are valid
pub fn parse_credential_payload(credential: &str) -> Option<[u8; SECRET_PAYLOAD_LEN]> {
    let rest = credential.strip_prefix(CREDENTIAL_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(rest).ok()?;
    bytes.try_into().ok()
}

/// Generates a versioned credential, display id, and digest.
///
/// # Flow
/// 1. Reads 32 CSPRNG bytes for the secret payload
/// 2. Encodes `opmx_v1_` plus unpadded base64url
/// 3. Hashes that string before the caller touches the database
/// 4. Reads 16 CSPRNG bytes for a safe `opk_` display identifier
///
/// # Parameters
/// - `entropy` - CSPRNG implementation
///
/// # Returns
/// Secret, digest, and display identifier
///
/// # Errors
/// Returns `Unavailable` when the CSPRNG cannot fill a buffer.
pub fn generate_credential(
    entropy: &dyn SecretSource,
) -> Result<GeneratedCredential, EntropyError> {
    let mut payload = [0_u8; SECRET_PAYLOAD_LEN];
    entropy.fill_bytes(&mut payload)?;
    let mut display_random = [0_u8; DISPLAY_ID_RANDOM_LEN];
    entropy.fill_bytes(&mut display_random)?;
    Ok(assemble_credential(payload, display_random))
}

/// Builds a credential from already-sampled random bytes.
///
/// # Parameters
/// - `payload` - 32-byte secret payload
/// - `display_random` - 16-byte display-id payload
///
/// # Returns
/// Versioned secret, SHA-256 digest, and display identifier
pub fn assemble_credential(
    payload: [u8; SECRET_PAYLOAD_LEN],
    display_random: [u8; DISPLAY_ID_RANDOM_LEN],
) -> GeneratedCredential {
    let credential = format!("{CREDENTIAL_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload));
    let digest = hash_credential(&credential);
    let display_id = format!("opk_{}", URL_SAFE_NO_PAD.encode(display_random));
    GeneratedCredential {
        credential: SecretString::new(credential),
        digest,
        display_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_credentials_use_versioned_32_byte_payload() {
        let first = generate_credential(&SystemEntropy).expect("entropy");
        let second = generate_credential(&SystemEntropy).expect("entropy");
        let first_secret = first.expose_secret().to_string();
        let second_secret = second.expose_secret().to_string();
        let first_payload = parse_credential_payload(&first_secret).expect("payload");
        let second_payload = parse_credential_payload(&second_secret).expect("payload");

        assert!(first_secret.starts_with(CREDENTIAL_PREFIX));
        assert!(second_secret.starts_with(CREDENTIAL_PREFIX));
        assert_eq!(first_payload.len(), SECRET_PAYLOAD_LEN);
        assert_eq!(second_payload.len(), SECRET_PAYLOAD_LEN);
        assert_ne!(first_secret, second_secret);
        assert_ne!(first.digest(), second.digest());
        assert_ne!(first.display_id(), second.display_id());
        assert_eq!(first.digest(), hash_credential(&first_secret));
        assert!(!first.display_id().contains(&first_secret));
        assert!(first.display_id().starts_with("opk_"));
    }

    #[test]
    fn many_generated_secrets_are_unique() {
        let mut secrets = HashSet::new();
        for _ in 0..8 {
            let generated = generate_credential(&SystemEntropy).expect("entropy");
            secrets.insert(generated.expose_secret().to_string());
        }
        assert_eq!(secrets.len(), 8);
    }

    #[test]
    fn debug_redacts_secret_and_digest_bytes() {
        let generated = assemble_credential([7; 32], [3; 16]);
        let secret = generated.expose_secret().to_string();
        let payload_b64 = secret
            .strip_prefix(CREDENTIAL_PREFIX)
            .expect("prefix")
            .to_string();
        let rendered = format!("{generated:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(rendered.contains("KeyDigest([redacted])"));
        assert!(!rendered.contains(&secret));
        assert!(!rendered.contains(&payload_b64));
        assert!(!rendered.contains("7, 7, 7"));
    }

    #[test]
    fn parse_rejects_malformed_credentials() {
        assert!(parse_credential_payload("not-a-key").is_none());
        assert!(parse_credential_payload("opmx_v1_???").is_none());
        assert!(parse_credential_payload("opmx_v2_abcd").is_none());
    }

    #[test]
    fn generator_uses_getrandom() {
        let source = include_str!("credentials.rs");
        let impl_source = source.split("mod tests").next().expect("implementation");
        assert!(impl_source.contains("getrandom::fill"));
        assert!(impl_source.contains("Sha256::digest"));
        assert!(!impl_source.contains("thread_rng"));
        assert!(!impl_source.contains("OsRng"));
    }
}
