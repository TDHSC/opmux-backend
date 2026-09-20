//! Estimated successful-response cost from configured target prices.

use super::{config::ModelPricing, error::ExecutorError};

const TOKENS_PER_MILLION: f64 = 1_000_000.0;
const COST_DECIMAL_PLACES: f64 = 100_000_000.0;

/// Estimates USD cost for one successful response.
///
/// Uses the selected target's configured per-million prices:
/// `prompt_tokens * input_per_million / 1_000_000 + completion_tokens *
/// output_per_million / 1_000_000`, rounded to 8 decimal places.
///
/// This is an estimate for the successful response only. It is not current
/// provider billing and does not account for retries or abandoned work.
/// Missing prices return an error instead of zero.
///
/// # Parameters
/// - `prompt_tokens` - Validated nonnegative prompt token count
/// - `completion_tokens` - Validated nonnegative completion token count
/// - `pricing` - Configured prices for the selected target
///
/// # Returns
/// Nonnegative finite USD estimate rounded to 8 decimal places
///
/// # Errors
/// Returns `MissingPricing` when prices are absent, and `InvalidUpstreamResult`
/// when token counts are negative or the estimate is not a finite nonnegative
/// number.
pub fn estimate_successful_response_cost(
    prompt_tokens: i64,
    completion_tokens: i64,
    pricing: Option<&ModelPricing>,
) -> Result<f64, ExecutorError> {
    let Some(pricing) = pricing else {
        return Err(ExecutorError::MissingPricing);
    };
    if prompt_tokens < 0 || completion_tokens < 0 {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    if !pricing.input_per_million.is_finite()
        || !pricing.output_per_million.is_finite()
        || pricing.input_per_million < 0.0
        || pricing.output_per_million < 0.0
    {
        return Err(ExecutorError::MissingPricing);
    }

    let cost = (prompt_tokens as f64) / TOKENS_PER_MILLION * pricing.input_per_million
        + (completion_tokens as f64) / TOKENS_PER_MILLION * pricing.output_per_million;
    if !cost.is_finite() || cost < 0.0 {
        return Err(ExecutorError::InvalidUpstreamResult);
    }
    Ok((cost * COST_DECIMAL_PLACES).round() / COST_DECIMAL_PLACES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices(input_per_million: f64, output_per_million: f64) -> ModelPricing {
        ModelPricing::new(input_per_million, output_per_million)
    }

    #[test]
    fn illustrative_primary_prices_yield_documented_cost() {
        let cost = estimate_successful_response_cost(120, 30, Some(&prices(1.0, 2.0)))
            .expect("priced estimate");
        assert_eq!(cost, 0.00018);
    }

    #[test]
    fn second_target_prices_are_used_for_the_same_usage() {
        let cost = estimate_successful_response_cost(120, 30, Some(&prices(0.25, 0.5)))
            .expect("priced estimate");
        assert_eq!(cost, 0.000045);
    }

    #[test]
    fn missing_prices_do_not_become_zero() {
        match estimate_successful_response_cost(120, 30, None) {
            Err(ExecutorError::MissingPricing) => {}
            other => panic!("expected MissingPricing, got {other:?}"),
        }
    }

    #[test]
    fn zero_tokens_with_configured_prices_may_be_zero() {
        let cost = estimate_successful_response_cost(0, 0, Some(&prices(1.0, 2.0)))
            .expect("zero-token estimate");
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn negative_usage_is_not_a_successful_estimate() {
        match estimate_successful_response_cost(-1, 30, Some(&prices(1.0, 2.0))) {
            Err(ExecutorError::InvalidUpstreamResult) => {}
            other => panic!("expected InvalidUpstreamResult, got {other:?}"),
        }
    }
}
