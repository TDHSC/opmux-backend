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
    build_client(timeout, false)
}

/// Builds a bounded HTTP client, disabling proxies for loopback targets.
///
/// Inherited `HTTP_PROXY`/`HTTPS_PROXY` settings must not redirect local
/// simulator traffic. Non-loopback URLs keep reqwest's default proxy policy.
///
/// # Parameters
/// - `timeout` - Finite connect and request timeout
/// - `base_url` - Provider base URL used to detect loopback targets
///
/// # Returns
/// Configured `reqwest::Client`
///
/// # Errors
/// Returns `invalid_limit` for a zero timeout and `http_client_construction`
/// when the client cannot be built.
pub fn build_bounded_http_client_for_base_url(
    timeout: Duration,
    base_url: &str,
) -> Result<reqwest::Client, ConfigError> {
    build_client(timeout, base_url_is_loopback(base_url))
}

fn build_client(
    timeout: Duration,
    disable_proxy: bool,
) -> Result<reqwest::Client, ConfigError> {
    if timeout.is_zero() {
        return Err(ConfigError::invalid_limit("max_attempt_timeout_ms"));
    }
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .tls_built_in_root_certs(true);
    if disable_proxy {
        builder = builder.no_proxy();
    }
    client_from_build_result(builder.build())
}

fn base_url_is_loopback(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    match url.host_str() {
        Some("localhost") | Some("127.0.0.1") | Some("::1") => true,
        Some(host) => host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false),
        None => false,
    }
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
        assert!(!production.contains("danger_accept_invalid_certs"));
        assert!(!production.contains("danger_accept_invalid_hostnames"));
        assert!(!production.contains("danger_accept_invalid"));
        assert!(production.contains("tls_built_in_root_certs(true)"));
        assert!(!production.contains("Client::new()"));
        assert!(production.contains("no_proxy()"));
    }

    #[test]
    fn loopback_base_urls_are_detected() {
        assert!(base_url_is_loopback("http://127.0.0.1:9/v1"));
        assert!(base_url_is_loopback("http://localhost:38081/v1"));
        assert!(!base_url_is_loopback("https://api.openai.com/v1"));
    }
}
