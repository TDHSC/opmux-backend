//! Bounded provider HTTP client construction.

use std::time::Duration;

use super::error::ConfigError;

/// Builds a bounded HTTP client with normal TLS verification enabled.
///
/// The client always has a finite timeout and never falls back to
/// reqwest's unbounded default client. Certificate and hostname verification
/// stay enabled; this helper never disables TLS verification.
pub fn build_bounded_http_client(
    timeout: Duration,
) -> Result<reqwest::Client, ConfigError> {
    if timeout.is_zero() {
        return Err(ConfigError::invalid_limit("max_attempt_timeout_ms"));
    }
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .tls_built_in_root_certs(true);
    client_from_build_result(builder.build())
}

/// Maps a client builder result without including builder diagnostics.
///
/// Direct tests use this to cover construction failure without depending on
/// a platform-specific TLS fault.
pub fn client_from_build_result<E>(
    result: Result<reqwest::Client, E>,
) -> Result<reqwest::Client, ConfigError> {
    result.map_err(|_| ConfigError::http_client_construction())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    struct FakeBuildError;

    impl fmt::Display for FakeBuildError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("tls failed for https://user:sentinel-secret@example.invalid")
        }
    }

    #[test]
    fn bounded_client_builds_with_positive_timeout() {
        assert!(build_bounded_http_client(Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn zero_timeout_is_rejected_before_client_build() {
        let error = build_bounded_http_client(Duration::ZERO).unwrap_err();
        assert_eq!(error.category(), "invalid_limit");
    }

    #[test]
    fn constructor_failure_is_sanitized_and_does_not_fallback() {
        let error =
            client_from_build_result::<FakeBuildError>(Err(FakeBuildError)).unwrap_err();
        assert_eq!(error.category(), "http_client_construction");
        let text = error.to_string();
        assert!(!text.contains("sentinel-secret"));
        assert!(!text.contains("https://"));
        assert!(!text.contains("user:"));
    }

    #[test]
    fn http_client_helper_source_keeps_tls_verification() {
        let production = include_str!("http_client.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        assert!(!production.contains("danger_accept_invalid"));
        assert!(production.contains("tls_built_in_root_certs(true)"));
        assert!(!production.contains("Client::new()"));
    }
}
