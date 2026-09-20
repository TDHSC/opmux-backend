//! Test-only mock repository retained so historical fixtures stay off the
//! runtime path.

use super::mockdata::{MockApiKeyInfo, MockAuthDataProvider};

/// Mock implementation used only by tests.
pub struct MockAuthRepository;

impl MockAuthRepository {
    pub fn new() -> Self {
        Self
    }

    pub async fn find_api_key_by_hash(&self, key_hash: &str) -> Option<MockApiKeyInfo> {
        MockAuthDataProvider::get_api_key_by_hash(key_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn find_api_key_by_hash_returns_some_for_known_key() {
        let repo = MockAuthRepository::new();
        let result = repo.find_api_key_by_hash("test-api-key-123").await;
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.id, "key_001");
        assert_eq!(info.client_id, "test-client-456");
        assert!(info.is_active);
    }

    #[tokio::test]
    async fn find_api_key_by_hash_returns_none_for_unknown_key() {
        let repo = MockAuthRepository::new();
        let result = repo.find_api_key_by_hash("does-not-exist").await;
        assert!(result.is_none());
    }
}
