//! Next-fire computation + the missed-fire policy over a [`ParsedCron`].
//!
//! `next_after` is a field-by-field forward search: it advances the candidate
//! [`CivilTime`] to the next instant whose every field is in its cron set,
//! correctly rolling minute → hour → day → month → year (and respecting the
//! Quartz day-of-month / day-of-week OR-semantics). The search is bounded by
//! [`MAX_YEAR`] so an impossible expression terminates.

use crate::parse::ParsedCron;
use crate::time::{days_in_month, CivilTime, MAX_YEAR};

/// What to do about fires that were missed while the clock was not advancing
/// (a suspend/resume, a long GC pause, a wall-clock jump).
///
/// Per phase3/10: the DEFAULT is skip-to-next; fire-once is the documented
/// opt-in (a divergence from any single Rust crate, made explicit here).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MissedFire {
    /// Skip every missed fire and resume at the next fire strictly after `now`
    /// (the default — async has no implicit backlog ceiling).
    #[default]
    SkipToNext,
    /// Fire exactly once to acknowledge the missed window, then resume the
    /// normal cadence (catch-up-once).
    FireOnce,
}

impl ParsedCron {
    /// The next fire strictly after `from`, or `None` if none exists before
    /// [`MAX_YEAR`].
    ///
    /// Strict: a `from` that itself matches does NOT re-fire — the search starts
    /// at `from + 1s`. This is the contract the scheduler wants (it asks "what's
    /// next?" holding the last fire time).
    #[must_use]
    pub fn next_after(&self, from: CivilTime) -> Option<CivilTime> {
        let mut t = from.next_second()?;
        // Bounded outer loop: each pass either matches or advances at least one
        // field; the year ceiling guarantees termination.
        loop {
            if t.year > MAX_YEAR {
                return None;
            }
            match self.advance_to_match(t) {
                Match::Hit(found) => return Some(found),
                Match::Retry(next) => t = next,
                Match::Exhausted => return None,
            }
        }
    }

    /// One field-by-field narrowing pass from `t`. Returns the matching instant,
    /// the next candidate to retry from, or exhaustion.
    fn advance_to_match(&self, t: CivilTime) -> Match {
        // 1) Year.
        let Some(year) = self.year.next_from(t.year as u32) else {
            return Match::Exhausted;
        };
        let year = year as i64;
        if year != t.year {
            // Jump to Jan 1 00:00:00 of that year and re-test.
            return match CivilTime::new(year, 1, 1, 0, 0, 0) {
                Some(c) => Match::Retry(c),
                None => Match::Exhausted,
            };
        }

        // 2) Month.
        let Some(month) = self.month.next_from(t.month as u32) else {
            // No month this year ⇒ jump to next year.
            return Match::Retry(reset_to(t.year + 1, 1, 1, 0, 0, 0));
        };
        let month = month as u8;
        if month != t.month {
            return Match::Retry(reset_to(t.year, month, 1, 0, 0, 0));
        }

        // 3) Day (respecting the dom/dow OR-rule). Find the first day >= t.day in
        //    this month that the calendar accepts.
        match self.next_matching_day(t.year, month, t.day) {
            DayPick::Day(day) => {
                if day != t.day {
                    return Match::Retry(reset_to(t.year, month, day, 0, 0, 0));
                }
            }
            DayPick::NextMonth => {
                // No more days this month ⇒ next month.
                return Match::Retry(advance_month(t.year, month));
            }
        }

        // 4) Hour.
        let Some(hour) = self.hour.next_from(t.hour as u32) else {
            // No hour this day ⇒ next day at 00:00:00.
            return Match::Retry(next_day(t.year, month, t.day));
        };
        let hour = hour as u8;
        if hour != t.hour {
            return Match::Retry(reset_to(t.year, month, t.day, hour, 0, 0));
        }

        // 5) Minute.
        let Some(minute) = self.minute.next_from(t.minute as u32) else {
            // No minute this hour ⇒ next hour at :00:00.
            return Match::Retry(next_hour(t.year, month, t.day, t.hour));
        };
        let minute = minute as u8;
        if minute != t.minute {
            return Match::Retry(reset_to(t.year, month, t.day, t.hour, minute, 0));
        }

        // 6) Second.
        let Some(second) = self.second.next_from(t.second as u32) else {
            // No second this minute ⇒ next minute at :00.
            return Match::Retry(next_minute(t.year, month, t.day, t.hour, t.minute));
        };
        let second = second as u8;
        if second != t.second {
            return Match::Retry(reset_to(t.year, month, t.day, t.hour, t.minute, second));
        }

        // All fields matched.
        Match::Hit(t)
    }

    /// The first day `>= from_day` in `(year, month)` that the dom/dow rule
    /// accepts, or `NextMonth` if none.
    fn next_matching_day(&self, year: i64, month: u8, from_day: u8) -> DayPick {
        let last = days_in_month(year, month);
        let mut day = from_day;
        while day <= last {
            if self.day_matches(year, month, day) {
                return DayPick::Day(day);
            }
            day += 1;
        }
        DayPick::NextMonth
    }

    /// The Quartz/Vixie day rule: if BOTH day-of-month and day-of-week are
    /// restricted, a day matches when EITHER does (OR); otherwise only the
    /// restricted one constrains (an unrestricted field is `*`).
    fn day_matches(&self, year: i64, month: u8, day: u8) -> bool {
        let dom_ok = self.day_of_month.contains(day as u32);
        let dow = CivilTime::new(year, month, day, 0, 0, 0)
            .map(CivilTime::day_of_week)
            .unwrap_or(0);
        let dow_ok = self.day_of_week.contains(dow as u32);

        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_ok || dow_ok,
            (true, false) => dom_ok,
            (false, true) => dow_ok,
            (false, false) => true,
        }
    }

    /// Whether `t` itself is an exact fire instant (every field in its set).
    #[must_use]
    pub fn matches(&self, t: CivilTime) -> bool {
        self.year.contains(t.year as u32)
            && self.month.contains(t.month as u32)
            && self.day_matches(t.year, t.month, t.day)
            && self.hour.contains(t.hour as u32)
            && self.minute.contains(t.minute as u32)
            && self.second.contains(t.second as u32)
    }

    /// Apply the [`MissedFire`] policy to compute the fire to schedule given the
    /// last fire `last` and the current time `now` (`now >= last`).
    ///
    /// - [`MissedFire::SkipToNext`]: the next fire strictly after `now`.
    /// - [`MissedFire::FireOnce`]: if any fire was due in `(last, now]`, return
    ///   the EARLIEST such missed fire (catch up once); otherwise the next fire
    ///   strictly after `now`.
    #[must_use]
    pub fn apply_missed(
        &self,
        last: CivilTime,
        now: CivilTime,
        policy: MissedFire,
    ) -> Option<CivilTime> {
        match policy {
            MissedFire::SkipToNext => self.next_after(now),
            MissedFire::FireOnce => {
                // The first fire strictly after `last`. If it is `<= now` a fire
                // was missed in the window and we catch up with exactly that
                // (earliest) one; if it is already in the future nothing was
                // missed and it is simply the next fire — same value either way.
                self.next_after(last)
            }
        }
    }
}

/// The result of one narrowing pass.
enum Match {
    /// Every field matched at this instant.
    Hit(CivilTime),
    /// Advance to this candidate and pass again.
    Retry(CivilTime),
    /// No match exists before the year ceiling.
    Exhausted,
}

/// The result of the day search within a month.
enum DayPick {
    /// This day matches.
    Day(u8),
    /// No matching day remains this month.
    NextMonth,
}

/// Build a civil time, clamping a too-large day to the month's last day so the
/// search never stalls on an out-of-range reset (it will be re-narrowed).
fn reset_to(year: i64, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> CivilTime {
    let day = day.min(days_in_month(year, month)).max(1);
    CivilTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

/// Move to the first instant of the next month after `(year, month)`.
fn advance_month(year: i64, month: u8) -> CivilTime {
    if month >= 12 {
        reset_to(year + 1, 1, 1, 0, 0, 0)
    } else {
        reset_to(year, month + 1, 1, 0, 0, 0)
    }
}

/// Move to 00:00:00 of the day after `(year, month, day)`.
fn next_day(year: i64, month: u8, day: u8) -> CivilTime {
    if day >= days_in_month(year, month) {
        advance_month(year, month)
    } else {
        reset_to(year, month, day + 1, 0, 0, 0)
    }
}

/// Move to :00:00 of the hour after `(... hour)`.
fn next_hour(year: i64, month: u8, day: u8, hour: u8) -> CivilTime {
    if hour >= 23 {
        next_day(year, month, day)
    } else {
        reset_to(year, month, day, hour + 1, 0, 0)
    }
}

/// Move to :00 of the minute after `(... minute)`.
fn next_minute(year: i64, month: u8, day: u8, hour: u8, minute: u8) -> CivilTime {
    if minute >= 59 {
        next_hour(year, month, day, hour)
    } else {
        reset_to(year, month, day, hour, minute + 1, 0)
    }
}
