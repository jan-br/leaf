//! A minimal pure civil-time representation for the cron calendar engine.
//!
//! The [`Trigger`](leaf_core::Trigger) SPI computes over opaque
//! [`Instant`](std::time::Instant)s, but cron is a CALENDAR cadence — it must
//! reason about year/month/day/hour/minute/second and the day-of-week. Rather
//! than pull in `chrono`/`time`, this module owns the small slice of proleptic
//! Gregorian arithmetic the engine needs, in `std` only.
//!
//! ## Documented contract (phase3/10 "Cron calendar semantics divergence" risk)
//!
//! - Time is **UTC / wall-clock with no DST or timezone shifts**. A field-by-field
//!   match is computed against this naive civil time; leaf-boot supplies the
//!   wall-clock anchor that maps an [`Instant`](std::time::Instant) to a
//!   [`CivilTime`]. There is therefore no DST "spring-forward skip / fall-back
//!   double-fire" handling — an explicit, documented divergence from Spring's
//!   `CronExpression` (which is zone-aware) and from some Rust cron crates.
//! - The day-of-week numbering is `0..=6` Sunday-based (`0`/`7` = Sunday), the
//!   Unix/Quartz-compatible convention. [`CivilTime::day_of_week`] returns it.
//! - The supported year range is the proleptic Gregorian calendar; the engine
//!   bounds its search at [`MAX_YEAR`] so a never-matching expression terminates.

/// The upper year bound the next-fire search will not cross (so an impossible
/// expression — e.g. Feb 30 — terminates with `None` rather than looping).
pub const MAX_YEAR: i64 = 9999;

/// A naive civil date-time (UTC / no timezone), to second resolution.
///
/// Fields are always in their natural calendar ranges (`month` `1..=12`,
/// `day` `1..=days_in_month`, `hour` `0..=23`, `minute`/`second` `0..=59`).
/// Construct via [`CivilTime::new`] (validated) or arithmetic helpers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CivilTime {
    /// Proleptic Gregorian year (e.g. `2026`).
    pub year: i64,
    /// Month of year, `1..=12`.
    pub month: u8,
    /// Day of month, `1..=31` (calendar-valid for the month/year).
    pub day: u8,
    /// Hour of day, `0..=23`.
    pub hour: u8,
    /// Minute of hour, `0..=59`.
    pub minute: u8,
    /// Second of minute, `0..=59`.
    pub second: u8,
}

impl CivilTime {
    /// Construct a civil time, returning `None` if any field is out of its
    /// calendar range (including a day past the month's length).
    #[must_use]
    pub fn new(year: i64, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<Self> {
        if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        if day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// The Sunday-based day-of-week (`0` = Sunday .. `6` = Saturday), via the
    /// days-from-civil-epoch count (Howard Hinnant's algorithm).
    #[must_use]
    pub fn day_of_week(self) -> u8 {
        // days_from_epoch is days since 1970-01-01, which was a Thursday (=4).
        let z = days_from_epoch(self.year, self.month, self.day);
        // (z + 4) mod 7 maps the Thursday epoch to Sunday-based 0..=6.
        (((z % 7) + 4 + 7 * 2) % 7) as u8
    }

    /// The next second after `self` (rolling minute/hour/day/month/year over).
    ///
    /// Returns `None` only if the increment would cross [`MAX_YEAR`].
    #[must_use]
    pub fn next_second(self) -> Option<Self> {
        let mut t = self;
        if t.second < 59 {
            t.second += 1;
            return Some(t);
        }
        t.second = 0;
        t.bump_minute()
    }

    fn bump_minute(mut self) -> Option<Self> {
        if self.minute < 59 {
            self.minute += 1;
            return Some(self);
        }
        self.minute = 0;
        self.bump_hour()
    }

    fn bump_hour(mut self) -> Option<Self> {
        if self.hour < 23 {
            self.hour += 1;
            return Some(self);
        }
        self.hour = 0;
        self.bump_day()
    }

    fn bump_day(mut self) -> Option<Self> {
        if self.day < days_in_month(self.year, self.month) {
            self.day += 1;
            return Some(self);
        }
        self.day = 1;
        self.bump_month()
    }

    fn bump_month(mut self) -> Option<Self> {
        if self.month < 12 {
            self.month += 1;
            return Some(self);
        }
        self.month = 1;
        self.year += 1;
        if self.year > MAX_YEAR {
            return None;
        }
        Some(self)
    }
}

/// Whether `year` is a Gregorian leap year.
#[must_use]
pub fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `month` of `year` (`month` `1..=12`).
#[must_use]
pub fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since the Unix epoch (1970-01-01) for a civil date — Howard Hinnant's
/// `days_from_civil`, valid across the proleptic Gregorian calendar.
#[must_use]
pub fn days_from_epoch(year: i64, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}
