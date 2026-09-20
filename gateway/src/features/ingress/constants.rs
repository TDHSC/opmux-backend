// Constants for ingress module

/// AI response role for assistant messages.
pub const AI_RESPONSE_ROLE: &str = "assistant";

/// Slow-request warning threshold in milliseconds.
pub const SLOW_REQUEST_THRESHOLD_MS: u64 = 1000;

/// Request validation limits. Prompt and metadata maxima match documented
/// `PolicyLimits` defaults; handlers use the injected limits.
pub const MIN_PROMPT_LENGTH: usize = 1;
pub const MAX_PROMPT_LENGTH: usize = 4000;
pub const MAX_METADATA_SIZE: usize = 1000;

/// Inclusive sampling temperature forwarded to the selected target.
pub const MIN_TEMPERATURE: f64 = 0.0;
/// Inclusive sampling temperature forwarded to the selected target.
pub const MAX_TEMPERATURE: f64 = 2.0;
/// Inclusive nucleus-sampling `top_p` forwarded to the selected target.
pub const MIN_TOP_P: f64 = 0.0;
/// Inclusive nucleus-sampling `top_p` forwarded to the selected target.
pub const MAX_TOP_P: f64 = 1.0;
