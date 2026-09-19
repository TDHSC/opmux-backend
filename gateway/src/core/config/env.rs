//! Environment lookup used by configuration loading.

use std::collections::HashMap;

/// Reads configuration values from a named environment source.
pub trait EnvSource {
    /// Returns the raw value when the key is present.
    fn get(&self, key: &str) -> Option<String>;
}

/// Process environment of the current runtime.
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvSource for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        HashMap::get(self, key).cloned()
    }
}
