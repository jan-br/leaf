//! Backoff policies + the reactive [`Sleeper`] seam (retry/resilience, phase3/09).
//!
//! leaf-core ships the PURE decision primitives ([`RetryPolicy`](leaf_core::RetryPolicy),
//! the [`BackoffPolicy`](leaf_core::BackoffPolicy) trait, [`FixedBackoff`](leaf_core::FixedBackoff),
//! and [`RetryTemplate::should_retry`](leaf_core::RetryTemplate::should_retry) — the
//! pure delay decision). This module adds the two backoff impls the design names
//! beyond Fixed — [`ExponentialBackoff`]`{base, mult, max, jitter}` and [`NoBackoff`] —
//! and the [`Sleeper`] seam the awaiting `execute` loop parks on.
//!
//! ## The reactive [`Sleeper`] seam (NO busy-poll)
//!
//! The design pins: "on a retryable error awaits `backoff.next_delay(attempt)` on the
//! ONE shared reactive timer-wheel (the sanctioned cold-path poll, never busy-wait)."
//! leaf-core has no runtime-agnostic async-sleep trait, so leaf-resilience declares
//! [`Sleeper`] — a one-method seam `sleep(Duration) -> BoxFuture` a runtime crate
//! (leaf-tokio, via `tokio::time::sleep`) implements. The interceptor body NEVER
//! spins: it `.await`s the sleeper's future, which parks on the runtime timer.
//!
//! [`ImmediateSleeper`] is the runtime-free default (it resolves instantly) — used by
//! the unit tests and by a zero-delay backoff; a production binding hands a
//! runtime-backed sleeper through [`RetryTemplate`](crate::RetryTemplate)`::with_sleeper`.

use std::sync::Arc;
use std::time::Duration;

use leaf_core::BoxFuture;

pub use leaf_core::{BackoffPolicy, FixedBackoff};

/// A reactive async-sleep seam (NO busy-poll): given a delay, return a future that
/// completes after it. A runtime crate parks on its timer (`tokio::time::sleep`);
/// the retry loop only ever `.await`s this — it never spins.
///
/// Object-safe (boxes its future) so it rides an `Arc<dyn Sleeper>` on the
/// [`RetryTemplate`](crate::RetryTemplate).
pub trait Sleeper: Send + Sync {
    /// A future that completes after `delay` (parked on a reactive timer).
    fn sleep(&self, delay: Duration) -> BoxFuture<'static, ()>;
}

/// The runtime-free [`Sleeper`]: completes IMMEDIATELY, ignoring the delay.
///
/// The default for unit tests and a zero-delay backoff — it does NOT busy-poll
/// (there is nothing to wait for; the future is `Ready` on first poll). A real
/// timed backoff binds a runtime-backed sleeper instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImmediateSleeper;

impl Sleeper for ImmediateSleeper {
    fn sleep(&self, _delay: Duration) -> BoxFuture<'static, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// The shared immediate sleeper as an `Arc<dyn Sleeper>` (the default the
/// template uses when none is bound).
#[must_use]
pub fn immediate_sleeper() -> Arc<dyn Sleeper> {
    Arc::new(ImmediateSleeper)
}

/// No backoff: retry IMMEDIATELY (zero delay) on every attempt while the policy
/// still permits one. (`next_delay` yields `Some(ZERO)` — the loop still consults
/// the [`RetryPolicy`](leaf_core::RetryPolicy) for `max_attempts`/retryability.)
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBackoff;

impl BackoffPolicy for NoBackoff {
    fn next_delay(&self, _attempt: u32) -> Option<Duration> {
        Some(Duration::ZERO)
    }
}

/// Exponential backoff with an optional cap + deterministic jitter
/// (retry/resilience): the delay before `attempt` (1-based) is
/// `base * mult^(attempt - 1)`, clamped to `max`, then reduced by up to
/// `jitter` fraction.
///
/// `jitter` is in `[0.0, 1.0]`: the delay is scaled by `1 - jitter * frac` where
/// `frac` is a deterministic per-attempt fraction in `[0, 1)` (a hashed,
/// dependency-free pseudo-random — full-jitter would need an RNG bean; this keeps
/// the policy pure + reproducible while still de-correlating retry storms). A
/// `jitter` of `0.0` is exact exponential.
#[derive(Clone, Copy, Debug)]
pub struct ExponentialBackoff {
    /// The base delay (the delay before the FIRST retry, attempt 1).
    pub base: Duration,
    /// The per-attempt multiplier (`>= 1.0`; `2.0` doubles each attempt).
    pub mult: f64,
    /// The maximum delay (the cap exponential growth saturates at).
    pub max: Duration,
    /// The jitter fraction in `[0.0, 1.0]` (`0.0` = exact exponential).
    pub jitter: f64,
}

impl ExponentialBackoff {
    /// An exponential backoff with the given base + multiplier, no cap (`max` is
    /// effectively unbounded) and no jitter.
    #[must_use]
    pub const fn new(base: Duration, mult: f64) -> Self {
        ExponentialBackoff { base, mult, max: Duration::MAX, jitter: 0.0 }
    }

    /// Cap the delay at `max` (builder style).
    #[must_use]
    pub const fn with_max(mut self, max: Duration) -> Self {
        self.max = max;
        self
    }

    /// Set the jitter fraction in `[0.0, 1.0]` (clamped; builder style).
    #[must_use]
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter.clamp(0.0, 1.0);
        self
    }
}

impl BackoffPolicy for ExponentialBackoff {
    fn next_delay(&self, attempt: u32) -> Option<Duration> {
        if attempt == 0 {
            return Some(Duration::ZERO);
        }
        // base * mult^(attempt - 1), computed in f64 seconds, saturating at `max`.
        let factor = self.mult.max(1.0).powi((attempt - 1) as i32);
        let raw = self.base.as_secs_f64() * factor;
        let max = self.max.as_secs_f64();
        let capped = if raw.is_finite() && raw < max { raw } else { max };
        // Deterministic jitter: scale by 1 - jitter * frac(attempt).
        let scaled = if self.jitter > 0.0 {
            capped * (1.0 - self.jitter * jitter_fraction(attempt))
        } else {
            capped
        };
        Some(Duration::from_secs_f64(scaled.max(0.0)))
    }
}

/// A deterministic per-attempt fraction in `[0, 1)` — a tiny dependency-free
/// integer hash of `attempt`, so jitter is reproducible (no RNG bean) yet
/// de-correlates a retry storm across attempts.
fn jitter_fraction(attempt: u32) -> f64 {
    // A SplitMix64-style finalizer over the attempt number.
    let mut z = (attempt as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Map the top 53 bits into [0, 1).
    ((z >> 11) as f64) / ((1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_backoff_is_zero_but_keeps_retrying() {
        let b = NoBackoff;
        assert_eq!(b.next_delay(1), Some(Duration::ZERO));
        assert_eq!(b.next_delay(5), Some(Duration::ZERO));
    }

    #[test]
    fn fixed_backoff_is_constant() {
        // Reusing leaf-core's FixedBackoff (one primitive, never re-minted).
        let b = FixedBackoff { delay: Duration::from_millis(50) };
        assert_eq!(b.next_delay(1), Some(Duration::from_millis(50)));
        assert_eq!(b.next_delay(9), Some(Duration::from_millis(50)));
    }

    #[test]
    fn exponential_doubles_each_attempt() {
        let b = ExponentialBackoff::new(Duration::from_millis(10), 2.0);
        // attempt 1 → base, attempt 2 → base*2, attempt 3 → base*4.
        assert_eq!(b.next_delay(1), Some(Duration::from_millis(10)));
        assert_eq!(b.next_delay(2), Some(Duration::from_millis(20)));
        assert_eq!(b.next_delay(3), Some(Duration::from_millis(40)));
    }

    #[test]
    fn exponential_saturates_at_max() {
        let b = ExponentialBackoff::new(Duration::from_millis(100), 10.0)
            .with_max(Duration::from_millis(250));
        assert_eq!(b.next_delay(1), Some(Duration::from_millis(100)));
        // 100 * 10 = 1000ms, capped to 250ms.
        assert_eq!(b.next_delay(2), Some(Duration::from_millis(250)));
        assert_eq!(b.next_delay(3), Some(Duration::from_millis(250)));
    }

    #[test]
    fn jitter_reduces_within_bound_and_is_deterministic() {
        let b = ExponentialBackoff::new(Duration::from_secs(1), 1.0).with_jitter(0.5);
        let d1 = b.next_delay(2).unwrap();
        let d2 = b.next_delay(2).unwrap();
        assert_eq!(d1, d2, "jitter is deterministic per attempt (reproducible)");
        // 0.5 jitter → delay in [0.5s, 1.0s].
        assert!(d1 <= Duration::from_secs(1), "jitter only REDUCES below the cap");
        assert!(d1 >= Duration::from_millis(500), "jitter bounded by the fraction");
    }

    #[test]
    fn jitter_clamps_out_of_range() {
        let b = ExponentialBackoff::new(Duration::from_secs(1), 1.0).with_jitter(5.0);
        assert!((b.jitter - 1.0).abs() < f64::EPSILON, "jitter clamps to 1.0");
    }

    #[test]
    fn immediate_sleeper_is_ready_at_once() {
        let s = ImmediateSleeper;
        // The future is Ready on first poll — no busy-poll, nothing to wait for.
        futures::executor::block_on(s.sleep(Duration::from_secs(3600)));
    }
}
