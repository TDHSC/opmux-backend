//! Process-environment isolation for local HTTP fixtures.

#![allow(dead_code)]

/// Names that must not leak inherited provider or proxy configuration into tests.
const PROVIDER_AND_PROXY_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_TIMEOUT_MS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "OPMUX_LIVE_PROVIDER_TESTS",
];

/// Clears inherited provider/proxy settings and disables auth bypass.
///
/// Tests then inject dummy credentials and loopback URLs explicitly. This
/// wrapper cannot activate live provider calls from an inherited key.
pub fn isolate_provider_environment() {
    for name in PROVIDER_AND_PROXY_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("NO_PROXY", "*");
    std::env::set_var("no_proxy", "*");
    std::env::set_var("AUTH_DEVELOPMENT_MODE", "false");
}
