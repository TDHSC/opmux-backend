// Constants for ingress module

/// AI response role for assistant messages.
pub const AI_RESPONSE_ROLE: &str = "assistant";

/// Slow-request warning threshold in milliseconds.
pub const SLOW_REQUEST_THRESHOLD_MS: u64 = 1000;

/// Request validation limits.
pub const MIN_PROMPT_LENGTH: usize = 1;
pub const MAX_PROMPT_LENGTH: usize = 4000;
pub const MAX_METADATA_SIZE: usize = 1000;
