//! Test-only mock API keys.
//!
//! These plaintext keys are never accepted by runtime authentication. They
//! exist so tests can prove the former public fixtures are rejected.

use std::collections::HashMap;

/// Test-only key metadata. Not a public DTO and not used at runtime.
#[derive(Debug, Clone)]
pub struct MockApiKeyInfo {
    pub id: String,
    pub client_id: String,
    pub is_active: bool,
}

/// Mock data provider retained only for tests.
pub struct MockAuthDataProvider;

impl MockAuthDataProvider {
    /// Looks up a historical plaintext mock key.
    pub fn get_api_key_by_hash(key_hash: &str) -> Option<MockApiKeyInfo> {
        Self::get_mock_api_keys().get(key_hash).cloned()
    }

    fn get_mock_api_keys() -> HashMap<String, MockApiKeyInfo> {
        let mut keys = HashMap::new();
        keys.insert(
            "test-api-key-123".to_string(),
            MockApiKeyInfo {
                id: "key_001".to_string(),
                client_id: "test-client-456".to_string(),
                is_active: true,
            },
        );
        keys.insert(
            "dev-api-key-456".to_string(),
            MockApiKeyInfo {
                id: "key_002".to_string(),
                client_id: "dev-client-789".to_string(),
                is_active: true,
            },
        );
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_keys_exist_only_as_test_fixtures() {
        assert!(MockAuthDataProvider::get_api_key_by_hash("test-api-key-123").is_some());
        assert!(MockAuthDataProvider::get_api_key_by_hash("dev-api-key-456").is_some());
        let service = include_str!("service.rs");
        let middleware = include_str!("../../middleware/auth.rs");
        assert!(!service.contains("test-api-key-123"));
        assert!(!service.contains("MockAuthDataProvider"));
        assert!(!middleware.contains("test-api-key-123"));
        assert!(!middleware.contains("create_dev_context"));
    }
}
