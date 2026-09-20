//! Validation of successful OpenAI Chat Completions payloads.

use super::{
    config::ModelPricing, error::ExecutorError, models::ExecutionResult,
    pricing::estimate_successful_response_cost,
};
use serde_json::Value;

const MALFORMED_JSON: &str = "malformed upstream JSON";

/// Parses a successful Chat Completions body into a validated result.
///
/// Malformed JSON is a protocol parse error. Empty choices, missing required
/// fields, non-text content, and impossible usage are invalid upstream results.
/// Neither case fabricates model, content, usage, or cost.
pub(crate) fn parse_successful_chat_completion(
    body: &[u8],
    pricing: Option<&ModelPricing>,
) -> Result<ExecutionResult, ExecutorError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| ExecutorError::JsonError(MALFORMED_JSON.to_string()))?;
    let object = value
        .as_object()
        .ok_or(ExecutorError::InvalidUpstreamResult)?;

    let model_used = required_nonempty_string(object.get("model"))?;
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .ok_or(ExecutorError::InvalidUpstreamResult)?;
    let choice = choices
        .first()
        .and_then(Value::as_object)
        .ok_or(ExecutorError::InvalidUpstreamResult)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or(ExecutorError::InvalidUpstreamResult)?;
    let content = required_string(message.get("content"))?;
    let role = required_nonempty_string(message.get("role"))?;
    let finish_reason = required_nonempty_string(choice.get("finish_reason"))?;
    let usage = object
        .get("usage")
        .and_then(Value::as_object)
        .ok_or(ExecutorError::InvalidUpstreamResult)?;
    let prompt_tokens = required_token_count(usage.get("prompt_tokens"))?;
    let completion_tokens = required_token_count(usage.get("completion_tokens"))?;
    let total_tokens = required_token_count(usage.get("total_tokens"))?;
    let expected_total = prompt_tokens
        .checked_add(completion_tokens)
        .ok_or(ExecutorError::InvalidUpstreamResult)?;
    if total_tokens != expected_total {
        return Err(ExecutorError::InvalidUpstreamResult);
    }

    let total_cost =
        estimate_successful_response_cost(prompt_tokens, completion_tokens, pricing)?;

    Ok(ExecutionResult {
        content,
        role,
        model_used,
        prompt_tokens,
        completion_tokens,
        total_cost,
        finish_reason,
    })
}

fn required_string(value: Option<&Value>) -> Result<String, ExecutorError> {
    match value {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(ExecutorError::InvalidUpstreamResult),
    }
}

fn required_nonempty_string(value: Option<&Value>) -> Result<String, ExecutorError> {
    let text = required_string(value)?;
    if text.is_empty() {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    Ok(text)
}

fn required_token_count(value: Option<&Value>) -> Result<i64, ExecutorError> {
    let Some(Value::Number(number)) = value else {
        return Err(ExecutorError::InvalidUpstreamResult);
    };
    let tokens = if let Some(value) = number.as_i64() {
        value
    } else if let Some(value) = number.as_u64() {
        i64::try_from(value).map_err(|_| ExecutorError::InvalidUpstreamResult)?
    } else {
        return Err(ExecutorError::InvalidUpstreamResult);
    };
    if tokens < 0 {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prices() -> ModelPricing {
        ModelPricing::new(1.0, 2.0)
    }

    fn valid_body() -> Value {
        json!({
            "id": "chatcmpl-local-fixture",
            "object": "chat.completion",
            "created": 0,
            "model": "reported-snapshot-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "SIMULATED_OPENAI_OK"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 30,
                "total_tokens": 150
            }
        })
    }

    fn parse_value(value: Value) -> Result<ExecutionResult, ExecutorError> {
        parse_successful_chat_completion(value.to_string().as_bytes(), Some(&prices()))
    }

    fn assert_invalid(result: Result<ExecutionResult, ExecutorError>) {
        match result {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            Err(ExecutorError::JsonError(_)) => {
                panic!("expected InvalidUpstreamResult, got JsonError")
            }
            Ok(result) => panic!(
                "fabricated success model={} content={} cost={}",
                result.model_used, result.content, result.total_cost
            ),
            Err(other) => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
    }

    #[test]
    fn valid_payload_uses_reported_model_and_priced_usage() {
        let result = parse_value(valid_body()).expect("valid payload");
        assert_eq!(result.content, "SIMULATED_OPENAI_OK");
        assert_eq!(result.role, "assistant");
        assert_eq!(result.finish_reason, "stop");
        assert_eq!(result.model_used, "reported-snapshot-model");
        assert_eq!(result.prompt_tokens, 120);
        assert_eq!(result.completion_tokens, 30);
        assert_eq!(result.total_cost, 0.00018);
    }

    #[test]
    fn malformed_json_is_a_protocol_parse_error() {
        let sentinel = b"{not-json RAW_UPSTREAM_SENTINEL";
        match parse_successful_chat_completion(sentinel, Some(&prices())) {
            Err(ExecutorError::JsonError(message)) => {
                assert_eq!(message, MALFORMED_JSON);
                assert!(!message.contains("RAW_UPSTREAM_SENTINEL"));
            }
            other => panic!("expected JsonError, got {other:?}"),
        }
    }

    #[test]
    fn empty_choices_cannot_become_success() {
        let mut body = valid_body();
        body["choices"] = json!([]);
        assert_invalid(parse_value(body));
    }

    #[test]
    fn missing_required_fields_cannot_become_success() {
        for field in ["model", "choices", "usage"] {
            let mut body = valid_body();
            body.as_object_mut().unwrap().remove(field);
            assert_invalid(parse_value(body));
        }

        let mut missing_content = valid_body();
        missing_content["choices"][0]["message"]
            .as_object_mut()
            .unwrap()
            .remove("content");
        assert_invalid(parse_value(missing_content));

        let mut missing_role = valid_body();
        missing_role["choices"][0]["message"]
            .as_object_mut()
            .unwrap()
            .remove("role");
        assert_invalid(parse_value(missing_role));

        let mut missing_finish = valid_body();
        missing_finish["choices"][0]
            .as_object_mut()
            .unwrap()
            .remove("finish_reason");
        assert_invalid(parse_value(missing_finish));
    }

    #[test]
    fn empty_required_strings_cannot_become_success() {
        let mut body = valid_body();
        body["model"] = json!("");
        assert_invalid(parse_value(body));
    }

    #[test]
    fn non_text_content_cannot_become_success() {
        let mut body = valid_body();
        body["choices"][0]["message"]["content"] =
            json!([{"type": "text", "text": "SIMULATED_OPENAI_OK"}]);
        assert_invalid(parse_value(body));
    }

    #[test]
    fn negative_and_fractional_usage_cannot_become_success() {
        let mut negative = valid_body();
        negative["usage"]["prompt_tokens"] = json!(-1);
        assert_invalid(parse_value(negative));

        let mut fractional = valid_body();
        fractional["usage"]["completion_tokens"] = json!(1.5);
        assert_invalid(parse_value(fractional));
    }

    #[test]
    fn inconsistent_total_tokens_cannot_become_success() {
        let mut body = valid_body();
        body["usage"]["total_tokens"] = json!(999);
        assert_invalid(parse_value(body));
    }

    #[test]
    fn valid_payload_still_parses_after_an_invalid_one() {
        assert_invalid(parse_value({
            let mut body = valid_body();
            body["choices"] = json!([]);
            body
        }));
        let result = parse_value(valid_body()).expect("later valid payload");
        assert_eq!(result.content, "SIMULATED_OPENAI_OK");
        assert_eq!(result.model_used, "reported-snapshot-model");
        assert_eq!(result.total_cost, 0.00018);
    }
}
