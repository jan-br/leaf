//! `leaf-cron` — the 6/7-field cron calendar engine consumed by leaf-core's
//! [`SchedulerCore`](leaf_core::SchedulerCore) / [`TriggerSpec::Cron`](leaf_core::TriggerSpec).
//!
//! Realizes the `scheduling` feature's cron piece (phase3/10, ADR-07 5e): own a
//! 6/7-field calendar engine, parsed at the Tier-2 startup validation pass (the
//! rare CPU-bound bit on a cold path). Pure + synchronous — no async, no runtime
//! named. The pieces:
//!
//! - [`parse`](mod@parse) — the [`ParsedCron`] field parser (`*`, `N`, `A-B`, `*/K`,
//!   `A-B/K`, `A/K`, comma lists, month/day-of-week names, the Quartz `?`).
//! - [`engine`] — [`ParsedCron::next_after`] (the field-by-field forward search,
//!   correct across minute/hour/day/month/year rollover and the Quartz dom/dow
//!   OR-rule) and the [`MissedFire`] policy.
//! - [`time`] — a minimal `std`-only naive civil time ([`CivilTime`]); the
//!   documented UTC / no-DST contract lives there.
//! - [`CronTrigger`] — the bridge that adapts the calendar engine to leaf-core's
//!   SYNC [`Trigger`] SPI over opaque
//!   [`Instant`]s, via a wall-clock anchor leaf-boot supplies.
//!
//! A malformed expression is a Tier-2 assembly failure ([`CronError`]) folded
//! into the ONE [`LeafError`](leaf_core::LeafError) chain (see [`error`]).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod engine;
pub mod error;
pub mod field;
pub mod parse;
pub mod time;

use std::time::{Duration, Instant};

use leaf_core::{Trigger, TriggerContext};

pub use engine::MissedFire;
pub use error::CronError;
pub use parse::{parse, ParsedCron};
pub use time::CivilTime;

/// A [`Trigger`] backed by the cron calendar engine.
///
/// leaf-core's `Trigger` SPI is intentionally calendar-blind — it computes over
/// opaque monotonic [`Instant`]s. Cron is a wall-clock
/// cadence, so this bridge holds an **anchor**: one `(Instant, CivilTime)` pair
/// pinning the monotonic clock to civil time (leaf-boot captures it once at
/// scheduler arm-time from the system wall clock). `next_fire` converts the
/// feedback instant to civil time, runs [`ParsedCron::next_after`], and converts
/// the result back to an `Instant`.
///
/// The conversion is naive (the anchor is assumed stable for the schedule's
/// lifetime); DST / wall-clock-jump correction is the documented divergence in
/// [`time`]. The [`MissedFire`] policy is applied when the feedback shows the
/// clock advanced past one or more fires between the last scheduled time and now.
#[derive(Clone, Debug)]
pub struct CronTrigger {
    expr: ParsedCron,
    /// The monotonic half of the anchor.
    anchor_instant: Instant,
    /// The civil-time half of the anchor (the wall clock at `anchor_instant`).
    anchor_civil: CivilTime,
    /// The missed-fire policy.
    missed: MissedFire,
}

impl CronTrigger {
    /// Build a trigger from a parsed expression and a wall-clock anchor.
    ///
    /// `anchor_instant`/`anchor_civil` pin the monotonic clock to civil time;
    /// the default [`MissedFire`] policy is [`MissedFire::SkipToNext`].
    #[must_use]
    pub fn new(expr: ParsedCron, anchor_instant: Instant, anchor_civil: CivilTime) -> Self {
        CronTrigger {
            expr,
            anchor_instant,
            anchor_civil,
            missed: MissedFire::SkipToNext,
        }
    }

    /// Parse `expr` and build a trigger with the given anchor.
    ///
    /// # Errors
    /// Returns a [`CronError`] if the expression does not parse.
    pub fn parse(
        expr: &str,
        anchor_instant: Instant,
        anchor_civil: CivilTime,
    ) -> Result<Self, CronError> {
        Ok(CronTrigger::new(parse(expr)?, anchor_instant, anchor_civil))
    }

    /// Set the [`MissedFire`] policy (builder style).
    #[must_use]
    pub fn with_missed_fire(mut self, missed: MissedFire) -> Self {
        self.missed = missed;
        self
    }

    /// The underlying parsed expression.
    #[must_use]
    pub fn expr(&self) -> &ParsedCron {
        &self.expr
    }

    /// Map an [`Instant`] onto civil time via the anchor (saturating: an instant
    /// before the anchor clamps to the anchor — the schedule never runs before
    /// it was armed).
    fn to_civil(&self, instant: Instant) -> CivilTime {
        let delta = instant.saturating_duration_since(self.anchor_instant);
        add_duration(self.anchor_civil, delta)
    }

    /// Map a civil time back to an [`Instant`] via the anchor. Returns `None`
    /// if the civil time precedes the anchor (unrepresentable on the monotonic
    /// clock without an earlier base).
    fn to_instant(&self, civil: CivilTime) -> Option<Instant> {
        let secs = civil_seconds_since(self.anchor_civil, civil)?;
        self.anchor_instant.checked_add(Duration::from_secs(secs))
    }
}

impl Trigger for CronTrigger {
    fn next_fire(&self, now: Instant, ctx: TriggerContext) -> Option<Instant> {
        // The reference point for "next" is the previous SCHEDULED fire when we
        // have one (so a cron cadence is stable), else `now`.
        let from_instant = ctx.last_scheduled.unwrap_or(now);
        let from_civil = self.to_civil(from_instant);
        let now_civil = self.to_civil(now);

        let fire = match (ctx.last_scheduled, self.missed) {
            // First fire, or skip-to-next: next strictly after `now`.
            (None, _) | (_, MissedFire::SkipToNext) => self.expr.next_after(now_civil),
            // Subsequent fire under fire-once: catch up from the last scheduled.
            (Some(_), MissedFire::FireOnce) => {
                self.expr.apply_missed(from_civil, now_civil, MissedFire::FireOnce)
            }
        }?;

        self.to_instant(fire)
    }
}

/// Add a [`Duration`] (whole seconds; sub-second is truncated — cron is
/// second-resolution) to a [`CivilTime`].
fn add_duration(base: CivilTime, delta: Duration) -> CivilTime {
    let mut secs = delta.as_secs();
    let mut t = base;
    // Bulk-advance by days first to keep the per-second loop bounded.
    let days = secs / 86_400;
    secs %= 86_400;
    for _ in 0..days {
        t = advance_one_day(t);
    }
    for _ in 0..secs {
        t = match t.next_second() {
            Some(n) => n,
            None => return t, // saturate at MAX_YEAR
        };
    }
    t
}

/// Advance one whole calendar day, preserving the time-of-day.
fn advance_one_day(t: CivilTime) -> CivilTime {
    let last = time::days_in_month(t.year, t.month);
    if t.day < last {
        CivilTime { day: t.day + 1, ..t }
    } else if t.month < 12 {
        CivilTime {
            month: t.month + 1,
            day: 1,
            ..t
        }
    } else {
        CivilTime {
            year: t.year + 1,
            month: 1,
            day: 1,
            ..t
        }
    }
}

/// Whole seconds from `from` to `to` (`to >= from`), or `None` if `to < from`.
fn civil_seconds_since(from: CivilTime, to: CivilTime) -> Option<u64> {
    if to < from {
        return None;
    }
    let day_diff = time::days_from_epoch(to.year, to.month, to.day)
        - time::days_from_epoch(from.year, from.month, from.day);
    let from_tod = tod_seconds(from);
    let to_tod = tod_seconds(to);
    let total = day_diff * 86_400 + to_tod - from_tod;
    u64::try_from(total).ok()
}

/// Seconds-since-midnight of a civil time.
fn tod_seconds(t: CivilTime) -> i64 {
    t.hour as i64 * 3600 + t.minute as i64 * 60 + t.second as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MissedFire;
    use crate::time::{days_from_epoch, days_in_month, is_leap_year, CivilTime};

    fn ct(y: i64, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> CivilTime {
        CivilTime::new(y, mo, d, h, mi, s).expect("valid civil time")
    }

    // ───────────────────────── time primitives ─────────────────────────

    #[test]
    fn leap_year_rules() {
        assert!(is_leap_year(2000)); // div 400
        assert!(!is_leap_year(1900)); // div 100 not 400
        assert!(is_leap_year(2024)); // div 4
        assert!(!is_leap_year(2026));
    }

    #[test]
    fn days_in_month_handles_february() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn day_of_week_is_sunday_based() {
        // 2026-06-14 is a Sunday; 2026-06-15 a Monday.
        assert_eq!(ct(2026, 6, 14, 0, 0, 0).day_of_week(), 0);
        assert_eq!(ct(2026, 6, 15, 0, 0, 0).day_of_week(), 1);
        // 1970-01-01 was a Thursday (=4).
        assert_eq!(ct(1970, 1, 1, 0, 0, 0).day_of_week(), 4);
    }

    #[test]
    fn days_from_epoch_anchors() {
        assert_eq!(days_from_epoch(1970, 1, 1), 0);
        assert_eq!(days_from_epoch(1970, 1, 2), 1);
        assert_eq!(days_from_epoch(1969, 12, 31), -1);
    }

    #[test]
    fn next_second_rolls_over_year_boundary() {
        let end = ct(2026, 12, 31, 23, 59, 59);
        assert_eq!(end.next_second(), Some(ct(2027, 1, 1, 0, 0, 0)));
    }

    #[test]
    fn invalid_civil_time_rejected() {
        assert!(CivilTime::new(2026, 2, 30, 0, 0, 0).is_none()); // Feb 30
        assert!(CivilTime::new(2026, 13, 1, 0, 0, 0).is_none()); // month 13
        assert!(CivilTime::new(2026, 1, 1, 24, 0, 0).is_none()); // hour 24
    }

    // ───────────────────────── parsing ─────────────────────────

    #[test]
    fn parse_every_minute() {
        // "0 * * * * *" = top of every minute.
        let c = parse("0 * * * * *").unwrap();
        assert!(c.second.contains(0));
        assert!(!c.second.contains(1));
        assert!(c.minute.contains(0));
        assert!(c.minute.contains(59));
        assert!(c.hour.contains(0));
        assert!(c.hour.contains(23));
    }

    #[test]
    fn parse_specific_time() {
        // 2:30:00 PM daily.
        let c = parse("0 30 14 * * *").unwrap();
        assert!(c.second.contains(0) && !c.second.contains(1));
        assert!(c.minute.contains(30) && !c.minute.contains(29));
        assert!(c.hour.contains(14) && !c.hour.contains(13));
    }

    #[test]
    fn parse_range() {
        let c = parse("0 0 9-17 * * *").unwrap();
        for h in 9..=17 {
            assert!(c.hour.contains(h), "hour {h}");
        }
        assert!(!c.hour.contains(8));
        assert!(!c.hour.contains(18));
    }

    #[test]
    fn parse_step() {
        // Every 15 seconds.
        let c = parse("*/15 * * * * *").unwrap();
        assert!(c.second.contains(0));
        assert!(c.second.contains(15));
        assert!(c.second.contains(30));
        assert!(c.second.contains(45));
        assert!(!c.second.contains(1));
    }

    #[test]
    fn parse_range_step() {
        // Minutes 0..=30 every 10 => 0,10,20,30.
        let c = parse("0 0-30/10 * * * *").unwrap();
        for m in [0u32, 10, 20, 30] {
            assert!(c.minute.contains(m), "minute {m}");
        }
        assert!(!c.minute.contains(40));
    }

    #[test]
    fn parse_from_step() {
        // "5/20" in seconds => 5, 25, 45.
        let c = parse("5/20 * * * * *").unwrap();
        for s in [5u32, 25, 45] {
            assert!(c.second.contains(s), "second {s}");
        }
        assert!(!c.second.contains(0));
    }

    #[test]
    fn parse_list() {
        let c = parse("0 0,15,30,45 * * * *").unwrap();
        for m in [0u32, 15, 30, 45] {
            assert!(c.minute.contains(m));
        }
        assert!(!c.minute.contains(10));
    }

    #[test]
    fn parse_month_names() {
        let c = parse("0 0 0 1 JAN-MAR *").unwrap();
        assert!(c.month.contains(1));
        assert!(c.month.contains(3));
        assert!(!c.month.contains(4));
    }

    #[test]
    fn parse_day_of_week_names_and_numbers() {
        // MON-FRI weekday restriction.
        let c = parse("0 0 0 ? * MON-FRI").unwrap();
        for d in 1..=5u32 {
            assert!(c.day_of_week.contains(d), "dow {d}");
        }
        assert!(!c.day_of_week.contains(0)); // Sunday
        assert!(!c.day_of_week.contains(6)); // Saturday
        assert!(c.dow_restricted);
        assert!(!c.dom_restricted);
    }

    #[test]
    fn parse_dow_seven_folds_to_sunday() {
        let c = parse("0 0 0 ? * 7").unwrap();
        assert!(c.day_of_week.contains(0), "7 must fold to Sunday(0)");
    }

    #[test]
    fn parse_seven_field_with_year() {
        let c = parse("0 0 0 1 1 ? 2030").unwrap();
        assert!(c.year.contains(2030));
        assert!(!c.year.contains(2031));
    }

    #[test]
    fn parse_question_mark_is_wildcard() {
        let c = parse("0 0 0 ? * *").unwrap();
        for d in 1..=28u32 {
            assert!(c.day_of_month.contains(d));
        }
        assert!(!c.dom_restricted);
    }

    // ── parse errors ──

    #[test]
    fn parse_rejects_wrong_field_count() {
        assert!(matches!(
            parse("* * * *"),
            Err(CronError::FieldCount { found: 4 })
        ));
        assert!(matches!(
            parse("* * * * * * * *"),
            Err(CronError::FieldCount { found: 8 })
        ));
    }

    #[test]
    fn parse_rejects_out_of_range() {
        assert!(matches!(
            parse("60 * * * * *"),
            Err(CronError::OutOfRange {
                field: "second",
                value: 60,
                ..
            })
        ));
        assert!(matches!(
            parse("0 0 24 * * *"),
            Err(CronError::OutOfRange { field: "hour", .. })
        ));
    }

    #[test]
    fn parse_rejects_inverted_range() {
        assert!(matches!(
            parse("0 0 17-9 * * *"),
            Err(CronError::InvertedRange { field: "hour", .. })
        ));
    }

    #[test]
    fn parse_rejects_zero_step() {
        assert!(matches!(
            parse("*/0 * * * * *"),
            Err(CronError::ZeroStep { field: "second" })
        ));
    }

    #[test]
    fn parse_rejects_question_in_wrong_field() {
        assert!(matches!(
            parse("? * * * * *"),
            Err(CronError::QuestionNotAllowed { field: "second" })
        ));
    }

    #[test]
    fn parse_rejects_bad_token() {
        assert!(matches!(
            parse("0 0 0 1 FOO *"),
            Err(CronError::BadToken { field: "month", .. })
        ));
    }

    #[test]
    fn cron_error_bridges_to_leaf_error() {
        let e = parse("* * * *").unwrap_err();
        let le: leaf_core::LeafError = e.into();
        assert_eq!(le.kind, leaf_core::ErrorKind::ConvertError);
    }

    // ───────────────────────── next-fire ─────────────────────────

    #[test]
    fn next_after_every_minute() {
        let c = parse("0 * * * * *").unwrap();
        // From 10:30:15, the next "second 0 of a minute" is 10:31:00.
        let next = c.next_after(ct(2026, 6, 15, 10, 30, 15)).unwrap();
        assert_eq!(next, ct(2026, 6, 15, 10, 31, 0));
    }

    #[test]
    fn next_after_is_strict() {
        let c = parse("0 * * * * *").unwrap();
        // Already at 10:31:00 => next is 10:32:00, NOT a re-fire of 10:31:00.
        let next = c.next_after(ct(2026, 6, 15, 10, 31, 0)).unwrap();
        assert_eq!(next, ct(2026, 6, 15, 10, 32, 0));
    }

    #[test]
    fn next_after_specific_daily_time_rolls_to_next_day() {
        let c = parse("0 0 9 * * *").unwrap(); // 09:00:00 daily
        // After today's fire, next is tomorrow 09:00:00.
        let next = c.next_after(ct(2026, 6, 15, 9, 0, 0)).unwrap();
        assert_eq!(next, ct(2026, 6, 16, 9, 0, 0));
    }

    #[test]
    fn next_after_crosses_month_boundary() {
        let c = parse("0 0 0 1 * *").unwrap(); // midnight on the 1st
        let next = c.next_after(ct(2026, 1, 15, 12, 0, 0)).unwrap();
        assert_eq!(next, ct(2026, 2, 1, 0, 0, 0));
    }

    #[test]
    fn next_after_crosses_year_boundary() {
        let c = parse("0 0 0 1 1 *").unwrap(); // Jan 1 midnight
        let next = c.next_after(ct(2026, 6, 15, 0, 0, 0)).unwrap();
        assert_eq!(next, ct(2027, 1, 1, 0, 0, 0));
    }

    #[test]
    fn next_after_leap_day() {
        // Feb 29 at midnight, only in leap years.
        let c = parse("0 0 0 29 2 *").unwrap();
        // From 2026 (non-leap), the next Feb 29 is 2028.
        let next = c.next_after(ct(2026, 3, 1, 0, 0, 0)).unwrap();
        assert_eq!(next, ct(2028, 2, 29, 0, 0, 0));
    }

    #[test]
    fn next_after_day_of_week() {
        // Every Monday at 00:00:00.
        let c = parse("0 0 0 ? * MON").unwrap();
        // 2026-06-15 is a Monday; from just after midnight, next Monday is 06-22.
        let next = c.next_after(ct(2026, 6, 15, 0, 0, 1)).unwrap();
        assert_eq!(next, ct(2026, 6, 22, 0, 0, 0));
    }

    #[test]
    fn next_after_dom_dow_or_semantics() {
        // Both restricted: the 15th OR any Friday. Quartz OR-rule.
        let c = parse("0 0 0 15 * FRI").unwrap();
        assert!(c.dom_restricted && c.dow_restricted);
        // From 2026-06-15 00:00:01 (a Monday-the-15th already passed midnight),
        // the next match is Friday 2026-06-19.
        let next = c.next_after(ct(2026, 6, 15, 0, 0, 1)).unwrap();
        assert_eq!(next, ct(2026, 6, 19, 0, 0, 0)); // Friday
    }

    #[test]
    fn next_after_seven_field_year_bounded() {
        // Only ever in 2030.
        let c = parse("0 0 0 1 1 ? 2030").unwrap();
        let next = c.next_after(ct(2026, 1, 1, 0, 0, 0)).unwrap();
        assert_eq!(next, ct(2030, 1, 1, 0, 0, 0));
        // After it fires, there is no next (year exhausted).
        assert!(c.next_after(ct(2030, 1, 1, 0, 0, 0)).is_none());
    }

    #[test]
    fn next_after_step_seconds() {
        let c = parse("*/15 * * * * *").unwrap();
        assert_eq!(
            c.next_after(ct(2026, 6, 15, 10, 0, 0)).unwrap(),
            ct(2026, 6, 15, 10, 0, 15)
        );
        assert_eq!(
            c.next_after(ct(2026, 6, 15, 10, 0, 50)).unwrap(),
            ct(2026, 6, 15, 10, 1, 0)
        );
    }

    #[test]
    fn matches_exact_instant() {
        let c = parse("0 30 14 * * *").unwrap();
        assert!(c.matches(ct(2026, 6, 15, 14, 30, 0)));
        assert!(!c.matches(ct(2026, 6, 15, 14, 31, 0)));
    }

    // ───────────────────────── missed-fire ─────────────────────────

    #[test]
    fn missed_fire_skip_to_next_ignores_backlog() {
        let c = parse("0 0 * * * *").unwrap(); // top of every hour
        let last = ct(2026, 6, 15, 1, 0, 0);
        // The clock jumped to 05:30 — we missed 02,03,04,05.
        let now = ct(2026, 6, 15, 5, 30, 0);
        let fire = c.apply_missed(last, now, MissedFire::SkipToNext).unwrap();
        // Skip the whole backlog, resume at the next fire after now: 06:00.
        assert_eq!(fire, ct(2026, 6, 15, 6, 0, 0));
    }

    #[test]
    fn missed_fire_fire_once_catches_up_earliest() {
        let c = parse("0 0 * * * *").unwrap();
        let last = ct(2026, 6, 15, 1, 0, 0);
        let now = ct(2026, 6, 15, 5, 30, 0);
        let fire = c.apply_missed(last, now, MissedFire::FireOnce).unwrap();
        // Fire exactly once for the earliest missed window: 02:00.
        assert_eq!(fire, ct(2026, 6, 15, 2, 0, 0));
    }

    #[test]
    fn missed_fire_fire_once_with_no_backlog_is_future() {
        let c = parse("0 0 * * * *").unwrap();
        let last = ct(2026, 6, 15, 1, 0, 0);
        // No clock jump: now is still before the next fire.
        let now = ct(2026, 6, 15, 1, 30, 0);
        let fire = c.apply_missed(last, now, MissedFire::FireOnce).unwrap();
        assert_eq!(fire, ct(2026, 6, 15, 2, 0, 0));
    }

    // ───────────────────────── Trigger bridge ─────────────────────────

    #[test]
    fn cron_trigger_first_fire_from_anchor() {
        let anchor_instant = Instant::now();
        let anchor_civil = ct(2026, 6, 15, 10, 0, 0);
        let t = CronTrigger::parse("0 * * * * *", anchor_instant, anchor_civil).unwrap();
        // First fire (no last_scheduled): next minute top = 10:01:00 = +60s.
        let next = t.next_fire(anchor_instant, TriggerContext::initial()).unwrap();
        assert_eq!(next, anchor_instant + Duration::from_secs(60));
    }

    #[test]
    fn cron_trigger_subsequent_fire_uses_last_scheduled() {
        let anchor_instant = Instant::now();
        let anchor_civil = ct(2026, 6, 15, 10, 0, 0);
        let t = CronTrigger::parse("0 * * * * *", anchor_instant, anchor_civil).unwrap();
        // last_scheduled = anchor + 60s (the 10:01:00 fire); next = 10:02:00.
        let last = anchor_instant + Duration::from_secs(60);
        let ctx = TriggerContext {
            last_scheduled: Some(last),
            ..TriggerContext::initial()
        };
        let next = t.next_fire(last, ctx).unwrap();
        assert_eq!(next, anchor_instant + Duration::from_secs(120));
    }

    #[test]
    fn cron_trigger_is_object_safe() {
        let t: Box<dyn Trigger> =
            Box::new(CronTrigger::parse("0 0 * * * *", Instant::now(), ct(2026, 6, 15, 0, 0, 0)).unwrap());
        assert!(t.next_fire(Instant::now(), TriggerContext::initial()).is_some());
    }

    #[test]
    fn cron_trigger_skip_to_next_default_on_clock_jump() {
        let anchor_instant = Instant::now();
        let anchor_civil = ct(2026, 6, 15, 1, 0, 0);
        let t = CronTrigger::parse("0 0 * * * *", anchor_instant, anchor_civil).unwrap();
        // last_scheduled at +0 (01:00); now jumped +4.5h to 05:30.
        let now = anchor_instant + Duration::from_secs((4 * 3600) + 1800);
        let ctx = TriggerContext {
            last_scheduled: Some(anchor_instant),
            ..TriggerContext::initial()
        };
        let next = t.next_fire(now, ctx).unwrap();
        // Default SkipToNext: resume at 06:00 = anchor + 5h.
        assert_eq!(next, anchor_instant + Duration::from_secs(5 * 3600));
    }

    #[test]
    fn cron_trigger_fire_once_catches_up() {
        let anchor_instant = Instant::now();
        let anchor_civil = ct(2026, 6, 15, 1, 0, 0);
        let t = CronTrigger::parse("0 0 * * * *", anchor_instant, anchor_civil)
            .unwrap()
            .with_missed_fire(MissedFire::FireOnce);
        let now = anchor_instant + Duration::from_secs((4 * 3600) + 1800); // 05:30
        let ctx = TriggerContext {
            last_scheduled: Some(anchor_instant), // 01:00
            ..TriggerContext::initial()
        };
        let next = t.next_fire(now, ctx).unwrap();
        // FireOnce catches up the earliest missed fire: 02:00 = anchor + 1h.
        assert_eq!(next, anchor_instant + Duration::from_secs(3600));
    }
}
