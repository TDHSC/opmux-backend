//! Shared attempt budget, capped exponential jitter, and Retry-After policy.
//!
//! One request owns a single actual-attempt counter across primary and
//! fallback hops. Per-attempt timeout is capped by remaining deadline time.
//! Exponential full jitter is capped by `backoff_cap_ms`. A valid provider
//! `Retry-After` is never shortened by that cap; if it cannot finish before
//! the deadline, the caller terminates the whole request as provider
//! throttling instead of claiming the deadline already elapsed or starting
//! later retries or configured fallbacks.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};

/// Counts actual provider calls started for one protected request.
#[derive(Debug, Clone)]
pub(crate) struct AttemptBudget {
    max_total: u32,
    started: u32,
}

impl AttemptBudget {
    /// Creates a budget that allows `max_total` started calls.
    ///
    /// # Parameters
    /// - `max_total` - Configured global actual-attempt ceiling
    pub(crate) fn new(max_total: u32) -> Self {
        Self {
            max_total: max_total.max(1),
            started: 0,
        }
    }

    /// Remaining slots before the global ceiling.
    pub(crate) fn remaining(&self) -> u32 {
        self.max_total.saturating_sub(self.started)
    }

    /// Number of calls already started.
    #[cfg(test)]
    pub(crate) fn started(&self) -> u32 {
        self.started
    }

    /// Records a started provider call when a slot remains.
    ///
    /// # Returns
    /// `true` when the call may start, `false` when the budget is exhausted
    pub(crate) fn try_start(&mut self) -> bool {
        if self.started >= self.max_total {
            false
        } else {
            self.started = self.started.saturating_add(1);
            true
        }
    }
}

/// Source of full-jitter samples in `0..=upper_inclusive` milliseconds.
pub(crate) trait JitterSource {
    /// Returns a delay in `0..=upper_inclusive` milliseconds.
    fn jitter_ms(&mut self, upper_inclusive: u64) -> u64;
}

/// Production jitter using the process CSPRNG.
pub(crate) struct SystemJitter;

impl JitterSource for SystemJitter {
    fn jitter_ms(&mut self, upper_inclusive: u64) -> u64 {
        if upper_inclusive == 0 {
            return 0;
        }
        let mut bytes = [0u8; 8];
        if getrandom::fill(&mut bytes).is_err() {
            return 0;
        }
        let raw = u64::from_le_bytes(bytes);
        let span = upper_inclusive.saturating_add(1);
        raw % span
    }
}

/// Scripted jitter for deterministic policy tests.
#[cfg(test)]
pub(crate) struct ScriptedJitter {
    values: std::vec::IntoIter<u64>,
}

#[cfg(test)]
impl ScriptedJitter {
    pub(crate) fn new(values: impl Into<Vec<u64>>) -> Self {
        Self {
            values: values.into().into_iter(),
        }
    }
}

#[cfg(test)]
impl JitterSource for ScriptedJitter {
    fn jitter_ms(&mut self, upper_inclusive: u64) -> u64 {
        self.values.next().unwrap_or(0).min(upper_inclusive)
    }
}

/// Planned wait before a later attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryDelay {
    /// Retry without sleeping.
    Immediate,
    /// Sleep `delay`. `provider_minimum` is true for a valid Retry-After.
    Sleep {
        delay: Duration,
        provider_minimum: bool,
    },
}

/// Why a planned wait cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelayError {
    /// Overall deadline would elapse during a jittered backoff.
    DeadlineExceeded,
    /// Provider Retry-After does not fit remaining time and must not be shortened.
    ProviderDelayCannotFit { retry_after: Duration },
}

/// Full-jitter exponential backoff, capped by `cap_ms`.
///
/// Retry 1 uses a 1000ms window, retry 2 uses 2000ms, then doubling, each
/// window clipped by `cap_ms` before sampling `0..=window`.
///
/// # Parameters
/// - `retry_index` - 1-based retry number after a failed attempt
/// - `cap_ms` - Configured backoff cap in milliseconds
/// - `rng` - Jitter source
///
/// # Returns
/// Delay in milliseconds
pub(crate) fn jittered_backoff_ms(
    retry_index: u32,
    cap_ms: u64,
    rng: &mut impl JitterSource,
) -> u64 {
    let exp = retry_index.saturating_sub(1);
    let base_ms = 1000_u64.saturating_mul(2_u64.saturating_pow(exp));
    let window = base_ms.min(cap_ms);
    rng.jitter_ms(window)
}

/// Chooses the wait before the next attempt.
///
/// A valid provider delay is used as-is and is not reduced by the backoff
/// cap. Missing or malformed Retry-After uses capped exponential jitter.
///
/// # Parameters
/// - `retry_index` - 1-based retry number
/// - `backoff_cap` - Configured jitter cap
/// - `provider_retry_after` - Parsed provider minimum wait, when valid
/// - `rng` - Jitter source used only when no provider delay is present
pub(crate) fn plan_retry_delay(
    retry_index: u32,
    backoff_cap: Duration,
    provider_retry_after: Option<Duration>,
    rng: &mut impl JitterSource,
) -> RetryDelay {
    if let Some(provider) = provider_retry_after {
        if provider.is_zero() {
            return RetryDelay::Immediate;
        }
        return RetryDelay::Sleep {
            delay: provider,
            provider_minimum: true,
        };
    }
    let delay = Duration::from_millis(jittered_backoff_ms(
        retry_index,
        duration_ms(backoff_cap),
        rng,
    ));
    if delay.is_zero() {
        RetryDelay::Immediate
    } else {
        RetryDelay::Sleep {
            delay,
            provider_minimum: false,
        }
    }
}

/// True when a valid provider Retry-After cannot finish in `remaining`.
///
/// Missing, malformed, or zero delays are not a provider minimum and must not
/// terminate the request as cannot-fit throttling.
///
/// # Parameters
/// - `provider_retry_after` - Parsed provider minimum wait, when valid
/// - `remaining` - Time left on the protected-request deadline
///
/// # Returns
/// `true` when the provider minimum exceeds remaining time
pub(crate) fn provider_minimum_cannot_fit(
    provider_retry_after: Option<Duration>,
    remaining: Duration,
) -> bool {
    let Some(delay) = provider_retry_after.filter(|delay| !delay.is_zero()) else {
        return false;
    };
    matches!(
        evaluate_retry_delay(
            RetryDelay::Sleep {
                delay,
                provider_minimum: true,
            },
            remaining,
        ),
        Err(DelayError::ProviderDelayCannotFit { .. })
    )
}

/// Bounds a planned wait by remaining deadline without shortening Retry-After.
///
/// # Parameters
/// - `delay` - Planned wait
/// - `remaining` - Time left on the protected-request deadline
///
/// # Returns
/// Duration to sleep, or a terminal delay error
///
/// # Errors
/// - `ProviderDelayCannotFit` when Retry-After exceeds remaining time
/// - `DeadlineExceeded` when jittered backoff cannot finish in time
pub(crate) fn evaluate_retry_delay(
    delay: RetryDelay,
    remaining: Duration,
) -> Result<Duration, DelayError> {
    match delay {
        RetryDelay::Immediate => Ok(Duration::ZERO),
        RetryDelay::Sleep {
            delay,
            provider_minimum,
        } => {
            if delay <= remaining {
                Ok(delay)
            } else if provider_minimum {
                Err(DelayError::ProviderDelayCannotFit { retry_after: delay })
            } else {
                Err(DelayError::DeadlineExceeded)
            }
        }
    }
}

/// Parses `Retry-After` as delta-seconds or HTTP-date.
///
/// Malformed values return `None` so callers use capped jitter instead of an
/// unbounded sleep. A date in the past is a zero delay, not a parse failure.
///
/// # Parameters
/// - `value` - Header value
/// - `now` - Clock used for HTTP-date conversion
///
/// # Returns
/// Parsed wait, or `None` when the value is malformed
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = trimmed.parse::<u64>().ok()?;
        return Some(Duration::from_secs(seconds));
    }
    let when = parse_http_date(trimmed)?;
    Some(when.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Parses a Retry-After header into milliseconds using `now`.
pub(crate) fn retry_after_header_ms(value: &str, now: SystemTime) -> Option<u64> {
    parse_retry_after(value, now).map(duration_ms)
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    if let Ok(parsed) = DateTime::parse_from_rfc2822(value) {
        return Some(SystemTime::from(parsed.with_timezone(&Utc)));
    }
    const FORMATS: &[&str] = &[
        "%a, %d %b %Y %H:%M:%S GMT",
        "%a, %e %b %Y %H:%M:%S GMT",
        "%A, %d-%b-%y %H:%M:%S GMT",
        "%a %b %e %H:%M:%S %Y",
    ];
    for format in FORMATS {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(SystemTime::from(naive.and_utc()));
        }
    }
    None
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn attempt_budget_never_replenishes() {
        let mut budget = AttemptBudget::new(3);
        assert!(budget.try_start());
        assert!(budget.try_start());
        assert!(budget.try_start());
        assert!(!budget.try_start());
        assert_eq!(budget.started(), 3);
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn considering_a_target_does_not_consume_a_slot() {
        let budget = AttemptBudget::new(2);
        assert_eq!(budget.started(), 0);
        assert_eq!(budget.remaining(), 2);
    }

    #[test]
    fn full_jitter_uses_exponential_windows_capped_at_two_seconds() {
        let cap = 2_000;
        let mut first = ScriptedJitter::new([0]);
        assert_eq!(jittered_backoff_ms(1, cap, &mut first), 0);

        let mut low = ScriptedJitter::new([250]);
        let mut high = ScriptedJitter::new([1_000]);
        assert_eq!(jittered_backoff_ms(1, cap, &mut low), 250);
        assert_eq!(jittered_backoff_ms(1, cap, &mut high), 1_000);

        let mut second = ScriptedJitter::new([2_000]);
        assert_eq!(jittered_backoff_ms(2, cap, &mut second), 2_000);

        let mut third = ScriptedJitter::new([2_000]);
        assert_eq!(
            jittered_backoff_ms(3, cap, &mut third),
            2_000,
            "base 4000ms must clip to the 2000ms cap before sampling"
        );
    }

    #[test]
    fn backoff_cap_does_not_shorten_provider_retry_after() {
        let mut rng = ScriptedJitter::new([2_000]);
        let delay = plan_retry_delay(
            1,
            Duration::from_millis(2_000),
            Some(Duration::from_secs(5)),
            &mut rng,
        );
        assert_eq!(
            delay,
            RetryDelay::Sleep {
                delay: Duration::from_secs(5),
                provider_minimum: true,
            }
        );
    }

    #[test]
    fn missing_retry_after_uses_capped_jitter() {
        let mut rng = ScriptedJitter::new([1_500]);
        let delay = plan_retry_delay(2, Duration::from_millis(2_000), None, &mut rng);
        assert_eq!(
            delay,
            RetryDelay::Sleep {
                delay: Duration::from_millis(1_500),
                provider_minimum: false,
            }
        );
    }

    #[test]
    fn delta_seconds_retry_after_parses() {
        let now = UNIX_EPOCH + Duration::from_secs(10);
        assert_eq!(parse_retry_after("2", now), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after(" 120 ", now),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn http_date_retry_after_uses_controlled_clock() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let later = now + Duration::from_secs(7);
        let header = DateTime::<Utc>::from(later)
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        assert_eq!(
            parse_retry_after(&header, now),
            Some(Duration::from_secs(7))
        );

        let past = DateTime::<Utc>::from(now - Duration::from_secs(30))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        assert_eq!(parse_retry_after(&past, now), Some(Duration::ZERO));
    }

    #[test]
    fn malformed_retry_after_is_ignored() {
        let now = UNIX_EPOCH + Duration::from_secs(1);
        assert_eq!(parse_retry_after("not-a-delay", now), None);
        assert_eq!(parse_retry_after("1.5", now), None);
        assert_eq!(parse_retry_after("2s", now), None);
        assert_eq!(parse_retry_after("", now), None);
        assert_eq!(parse_retry_after("-1", now), None);
    }

    #[test]
    fn provider_delay_that_cannot_fit_is_throttling_not_deadline() {
        let delay = RetryDelay::Sleep {
            delay: Duration::from_secs(30),
            provider_minimum: true,
        };
        match evaluate_retry_delay(delay, Duration::from_millis(400)) {
            Err(DelayError::ProviderDelayCannotFit { retry_after }) => {
                assert_eq!(retry_after, Duration::from_secs(30));
            }
            other => panic!("expected ProviderDelayCannotFit, got {other:?}"),
        }
    }

    #[test]
    fn jitter_that_cannot_fit_is_deadline_not_throttling() {
        let delay = RetryDelay::Sleep {
            delay: Duration::from_secs(2),
            provider_minimum: false,
        };
        assert_eq!(
            evaluate_retry_delay(delay, Duration::from_millis(200)),
            Err(DelayError::DeadlineExceeded)
        );
    }

    #[test]
    fn delay_that_fits_is_slept_in_full() {
        let delay = RetryDelay::Sleep {
            delay: Duration::from_millis(250),
            provider_minimum: true,
        };
        assert_eq!(
            evaluate_retry_delay(delay, Duration::from_secs(1)),
            Ok(Duration::from_millis(250))
        );
    }

    #[test]
    fn provider_minimum_cannot_fit_ignores_missing_or_zero_delays() {
        assert!(!provider_minimum_cannot_fit(None, Duration::from_millis(1)));
        assert!(!provider_minimum_cannot_fit(
            Some(Duration::ZERO),
            Duration::from_millis(1)
        ));
        assert!(!provider_minimum_cannot_fit(
            Some(Duration::from_millis(200)),
            Duration::from_secs(1)
        ));
        assert!(provider_minimum_cannot_fit(
            Some(Duration::from_secs(30)),
            Duration::from_millis(400)
        ));
    }
}
