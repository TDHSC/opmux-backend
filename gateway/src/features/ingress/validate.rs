//! Canonical ingress request parsing and validation.

use super::{
    constants::{MAX_TEMPERATURE, MAX_TOP_P, MIN_TEMPERATURE, MIN_TOP_P},
    error::IngressError,
    service::{GenerationParameters, IngressRequest},
};
use crate::core::config::PolicyLimits;
use serde_json::{Number, Value};

/// Parses and validates a `POST /api/v1/route` JSON object.
///
/// Unknown top-level controls and unsupported `stream`/`rewrite` fields are
/// rejected. Prompt bounds use the original untrimmed string. Parameter range
/// checks happen here; the selected target token cap is enforced after routing.
///
/// # Parameters
/// - `body` - Decoded JSON body
/// - `limits` - Injected prompt and metadata bounds
///
/// # Returns
/// Typed ingress request with original prompt and accepted generation options
///
/// # Errors
/// Returns `InvalidRequest` for missing fields, wrong types, unknown controls,
/// unsupported rewrite/stream flags, empty prompts, and out-of-range values.
pub fn parse_ingress_request(
    body: &Value,
    limits: &PolicyLimits,
) -> Result<IngressRequest, IngressError> {
    let object = body.as_object().ok_or_else(|| {
        IngressError::InvalidRequest("request must be a JSON object".to_string())
    })?;

    let mut prompt = None;
    let mut metadata = None;
    let mut route = None;
    let mut allow_fallback = None;
    let mut parameters = GenerationParameters::default();

    for (key, value) in object {
        match key.as_str() {
            "prompt" => prompt = Some(parse_prompt(value)?),
            "metadata" => metadata = Some(value.clone()),
            "route" => route = Some(parse_string_control(value, "route")?),
            "allow_fallback" => {
                allow_fallback = Some(parse_bool_control(value, "allow_fallback")?)
            }
            "parameters" => parameters = parse_parameters(value)?,
            "stream" => {
                return Err(IngressError::InvalidRequest(
                    "stream is not supported".to_string(),
                ));
            }
            "rewrite" => {
                return Err(IngressError::InvalidRequest(
                    "rewrite is not supported".to_string(),
                ));
            }
            _ => {
                return Err(IngressError::InvalidRequest("unknown control".to_string()));
            }
        }
    }

    let prompt = prompt
        .ok_or_else(|| IngressError::InvalidRequest("prompt is required".to_string()))?;
    let metadata = metadata.ok_or_else(|| {
        IngressError::InvalidRequest("metadata is required".to_string())
    })?;

    validate_prompt_bounds(&prompt, limits)?;
    validate_metadata_size(&metadata, limits)?;

    Ok(IngressRequest {
        prompt,
        metadata,
        route,
        allow_fallback,
        parameters,
    })
}

fn parse_prompt(value: &Value) -> Result<String, IngressError> {
    value.as_str().map(str::to_string).ok_or_else(|| {
        IngressError::InvalidRequest("prompt must be a string".to_string())
    })
}

fn parse_string_control(value: &Value, field: &str) -> Result<String, IngressError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| IngressError::InvalidRequest(format!("{field} must be a string")))
}

fn parse_bool_control(value: &Value, field: &str) -> Result<bool, IngressError> {
    value
        .as_bool()
        .ok_or_else(|| IngressError::InvalidRequest(format!("{field} must be a boolean")))
}

fn parse_parameters(value: &Value) -> Result<GenerationParameters, IngressError> {
    let object = value.as_object().ok_or_else(|| {
        IngressError::InvalidRequest("parameters must be an object".to_string())
    })?;

    let mut parameters = GenerationParameters::default();
    for (key, value) in object {
        match key.as_str() {
            "temperature" => {
                parameters.temperature = Some(parse_bounded_number(
                    value,
                    "temperature",
                    MIN_TEMPERATURE,
                    MAX_TEMPERATURE,
                )?);
            }
            "top_p" => {
                parameters.top_p =
                    Some(parse_bounded_number(value, "top_p", MIN_TOP_P, MAX_TOP_P)?);
            }
            "max_tokens" => parameters.max_tokens = Some(parse_max_tokens(value)?),
            _ => {
                return Err(IngressError::InvalidRequest(
                    "unknown parameter".to_string(),
                ));
            }
        }
    }
    Ok(parameters)
}

fn parse_bounded_number(
    value: &Value,
    field: &str,
    min: f64,
    max: f64,
) -> Result<Number, IngressError> {
    let number = match value {
        Value::Number(number) => number.clone(),
        _ => {
            return Err(IngressError::InvalidRequest(format!(
                "{field} must be a number"
            )));
        }
    };
    let parsed = number.as_f64().ok_or_else(|| {
        IngressError::InvalidRequest(format!("{field} must be a number"))
    })?;
    if !parsed.is_finite() || !(min..=max).contains(&parsed) {
        return Err(IngressError::InvalidRequest(format!(
            "{field} must be between {min:.1} and {max:.1}"
        )));
    }
    Ok(number)
}

fn parse_max_tokens(value: &Value) -> Result<u32, IngressError> {
    let parsed = match value {
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
    .and_then(|n| u32::try_from(n).ok())
    .filter(|n| *n > 0)
    .ok_or_else(|| {
        IngressError::InvalidRequest("max_tokens must be a positive integer".to_string())
    })?;
    Ok(parsed)
}

fn validate_prompt_bounds(
    prompt: &str,
    limits: &PolicyLimits,
) -> Result<(), IngressError> {
    if prompt.trim().is_empty() {
        return Err(IngressError::InvalidRequest(
            "Prompt cannot be empty".to_string(),
        ));
    }

    let char_count = prompt.chars().count() as u64;
    if char_count > limits.max_prompt_chars {
        return Err(IngressError::InvalidRequest(format!(
            "Prompt exceeds maximum length of {} characters",
            limits.max_prompt_chars
        )));
    }

    let byte_count = prompt.len() as u64;
    if byte_count > limits.max_prompt_chars {
        return Err(IngressError::InvalidRequest(format!(
            "Prompt exceeds maximum size of {} bytes",
            limits.max_prompt_chars
        )));
    }

    Ok(())
}

fn validate_metadata_size(
    metadata: &Value,
    limits: &PolicyLimits,
) -> Result<(), IngressError> {
    let metadata_size = serde_json::to_vec(metadata)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(u64::MAX);
    if metadata_size > limits.max_metadata_bytes {
        return Err(IngressError::InvalidRequest(format!(
            "Metadata exceeds maximum size of {} bytes",
            limits.max_metadata_bytes
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::PolicyLimits;
    use serde_json::{json, Map};

    fn limits_with_prompt(max_prompt_chars: u64) -> PolicyLimits {
        let mut limits = PolicyLimits::documented_defaults();
        limits.max_prompt_chars = max_prompt_chars;
        limits
    }

    fn parse(body: Value) -> Result<IngressRequest, IngressError> {
        parse_ingress_request(&body, &PolicyLimits::documented_defaults())
    }

    fn invalid(body: Value) -> String {
        match parse(body) {
            Err(IngressError::InvalidRequest(message)) => message,
            other => panic!("expected invalid request, got success={}", other.is_ok()),
        }
    }

    #[test]
    fn prompt_and_metadata_use_documented_defaults() {
        let request = parse(json!({
            "prompt": "hello",
            "metadata": {}
        }))
        .expect("valid default request");
        assert_eq!(request.prompt, "hello");
        assert_eq!(request.metadata, json!({}));
        assert_eq!(request.route, None);
        assert_eq!(request.allow_fallback, None);
        assert!(request.parameters.temperature.is_none());
        assert!(request.parameters.top_p.is_none());
        assert!(request.parameters.max_tokens.is_none());
    }

    #[test]
    fn accepted_numeric_endpoints_preserve_json_numbers() {
        let request = parse(json!({
            "prompt": "hello",
            "metadata": {"trace": true},
            "parameters": {
                "temperature": 0.0,
                "top_p": 1.0,
                "max_tokens": 32
            }
        }))
        .expect("valid endpoints");
        assert_eq!(
            request.parameters.temperature.clone().map(Value::Number),
            Some(json!(0.0))
        );
        assert_eq!(
            request.parameters.top_p.clone().map(Value::Number),
            Some(json!(1.0))
        );
        assert_eq!(request.parameters.max_tokens, Some(32));
    }

    #[test]
    fn original_prompt_bounds_reject_padding_and_keep_accepted_text() {
        let limits = limits_with_prompt(8);
        let accepted = parse_ingress_request(
            &json!({ "prompt": " ab cd ", "metadata": {} }),
            &limits,
        )
        .expect("padded-but-in-bound prompt");
        assert_eq!(accepted.prompt, " ab cd ");

        match parse_ingress_request(
            &json!({ "prompt": "abcdefgh ", "metadata": {} }),
            &limits,
        ) {
            Err(IngressError::InvalidRequest(message)) => {
                assert!(message.contains("characters"));
            }
            other => panic!(
                "expected padded over-limit failure, got success={}",
                other.is_ok()
            ),
        }

        match parse_ingress_request(
            &json!({ "prompt": "ééééé", "metadata": {} }),
            &limits,
        ) {
            Err(IngressError::InvalidRequest(message)) => {
                assert!(message.contains("bytes"));
            }
            other => panic!("expected over-byte failure, got success={}", other.is_ok()),
        }
    }

    #[test]
    fn unknown_and_unsupported_controls_are_rejected() {
        assert_eq!(
            invalid(json!({ "prompt": "hello", "metadata": {}, "model": "x" })),
            "unknown control"
        );
        assert_eq!(
            invalid(json!({ "prompt": "hello", "metadata": {}, "stream": true })),
            "stream is not supported"
        );
        assert_eq!(
            invalid(json!({ "prompt": "hello", "metadata": {}, "rewrite": true })),
            "rewrite is not supported"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "n": 1 }
            })),
            "unknown parameter"
        );
    }

    #[test]
    fn wrong_types_and_ranges_are_rejected() {
        assert_eq!(invalid(json!({ "metadata": {} })), "prompt is required");
        assert_eq!(
            invalid(json!({ "prompt": "hello" })),
            "metadata is required"
        );
        assert_eq!(
            invalid(json!({ "prompt": 1, "metadata": {} })),
            "prompt must be a string"
        );
        assert_eq!(
            invalid(json!({ "prompt": " \n", "metadata": {} })),
            "Prompt cannot be empty"
        );
        assert_eq!(
            invalid(json!({ "prompt": "hello", "metadata": {}, "route": 1 })),
            "route must be a string"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "allow_fallback": "false"
            })),
            "allow_fallback must be a boolean"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "temperature": "0.5" }
            })),
            "temperature must be a number"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "temperature": 2.1 }
            })),
            "temperature must be between 0.0 and 2.0"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "top_p": true }
            })),
            "top_p must be a number"
        );
        assert_eq!(
            invalid(json!({
                "prompt": "hello",
                "metadata": {},
                "parameters": { "max_tokens": 0 }
            })),
            "max_tokens must be a positive integer"
        );
    }

    #[test]
    fn fractional_max_tokens_is_rejected() {
        let body: Value = serde_json::from_str(
            r#"{"prompt":"hello","metadata":{},"parameters":{"max_tokens":1.5}}"#,
        )
        .expect("raw fractional JSON");
        assert_eq!(invalid(body), "max_tokens must be a positive integer");
    }

    #[test]
    fn object_key_order_does_not_affect_required_fields() {
        let mut object = Map::new();
        object.insert("metadata".to_string(), json!({}));
        object.insert("prompt".to_string(), json!("hello"));
        let request = parse(Value::Object(object)).expect("required fields");
        assert_eq!(request.prompt, "hello");
    }

    #[test]
    fn metadata_serialized_byte_bound_is_inclusive() {
        let mut limits = PolicyLimits::documented_defaults();
        limits.max_metadata_bytes = 8;
        let exact = json!({ "k": "" });
        assert_eq!(serde_json::to_vec(&exact).expect("exact").len(), 8);
        parse_ingress_request(&json!({ "prompt": "hello", "metadata": exact }), &limits)
            .expect("exact metadata bound");

        let over = json!({ "k": "a" });
        assert!(serde_json::to_vec(&over).expect("over").len() > 8);
        match parse_ingress_request(
            &json!({ "prompt": "hello", "metadata": over }),
            &limits,
        ) {
            Err(IngressError::InvalidRequest(message)) => {
                assert!(message.contains("Metadata exceeds maximum size of 8 bytes"));
            }
            other => panic!(
                "expected over-limit metadata failure, got success={}",
                other.is_ok()
            ),
        }
    }
}
