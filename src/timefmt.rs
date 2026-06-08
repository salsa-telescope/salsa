use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// Extension trait for rendering a UTC timestamp in a user's chosen
/// timezone. Used from Askama templates as `dt.in_tz(tz)` so every
/// date/time rendering path shares one conversion. `Tz` is `Copy`, so it
/// passes cleanly as a by-value template argument, and the resulting
/// `DateTime<Tz>` formats with `%Z` to show the local zone abbreviation.
pub trait InTz {
    fn in_tz(&self, tz: &Tz) -> DateTime<Tz>;
}

impl InTz for DateTime<Utc> {
    fn in_tz(&self, tz: &Tz) -> DateTime<Tz> {
        self.with_timezone(tz)
    }
}
