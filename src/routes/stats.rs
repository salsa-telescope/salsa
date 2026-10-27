//! Public usage statistics: totals per calendar month, never per user.
//!
//! Figures are computed live from the database on each request; the tables
//! are small enough that this takes a few milliseconds. Only completed
//! months are shown, and bookings and observations by admins (tests and
//! maintenance) are left out everywhere.

use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::{
    Extension, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use crate::app::AppState;
use crate::i18n::Language;
use crate::models::booking::Booking;
use crate::models::guest::GUEST_SESSION_HARD_CEILING_SECS;
use crate::models::stats::{RawStats, fetch};
use crate::models::user::User;
use crate::routes::admin::{count_booking_segments, without_admin_bookings};
use crate::routes::index::render_main;

/// The web-based control system launched in May 2026. Anything older in
/// the database is development and test data.
const FIRST_MONTH: Month = Month {
    year: 2026,
    month: 5,
};

/// Countries listed by name; the rest are folded into "Other".
const TOP_COUNTRIES: usize = 10;

pub fn routes(state: AppState) -> Router {
    Router::new().route("/", get(get_stats)).with_state(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Month {
    year: i32,
    month: u32,
}

impl Month {
    fn of_timestamp(ts: i64) -> Option<Month> {
        let date = chrono::DateTime::from_timestamp(ts, 0)?.date_naive();
        Some(Month {
            year: date.year(),
            month: date.month(),
        })
    }

    fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("Month is always valid")
    }

    fn next(self) -> Month {
        if self.month == 12 {
            Month {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Month {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    fn days(self) -> i64 {
        (self.next().first_day() - self.first_day()).num_days()
    }

    fn start_timestamp(self) -> i64 {
        Utc.from_utc_datetime(&self.first_day().and_hms_opt(0, 0, 0).unwrap())
            .timestamp()
    }

    /// As used in the query string: `2026-05`.
    fn param(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    fn parse(s: &str) -> Option<Month> {
        let (y, m) = s.trim().split_once('-')?;
        let month = Month {
            year: y.parse().ok()?,
            month: m.parse().ok()?,
        };
        (1..=12).contains(&month.month).then_some(month)
    }

    fn format(self, pattern: &str, lang: Language) -> String {
        capitalize(
            &self
                .first_day()
                .format_localized(pattern, lang.chrono_locale())
                .to_string(),
        )
    }
}

/// Swedish month names are lower case; they start table rows and chart
/// labels here, where a capital reads better.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Up to a year of bars each get a month name. Beyond that the names no
/// longer fit, so only years are marked: the first bar, and each January.
/// The first bar is left blank when a January follows within two months,
/// where the two labels would overlap. Every bar keeps its full tooltip.
fn bar_label(m: Month, index: usize, count: usize, lang: Language) -> String {
    if count <= 12 {
        m.format("%b", lang)
    } else if m.month == 1 {
        m.year.to_string()
    } else if index == 0 && m.month <= 10 {
        m.format("%b %Y", lang)
    } else {
        String::new()
    }
}

/// Completed months from the launch up to and including the month before
/// `today`'s.
fn completed_months(today: NaiveDate) -> Vec<Month> {
    let current = Month {
        year: today.year(),
        month: today.month(),
    };
    let mut months = Vec::new();
    let mut m = FIRST_MONTH;
    while m < current {
        months.push(m);
        m = m.next();
    }
    months
}

/// Everything counted for one month.
#[derive(Default)]
struct MonthData {
    bookings: Vec<Booking>,
    booked_seconds: i64,
    users: HashSet<i64>,
    /// (user, country) pairs, so a range can count each user once per country.
    user_countries: HashSet<(i64, String)>,
    guest_sessions: usize,
    guest_seconds: i64,
    guest_countries: HashMap<String, usize>,
    observations: usize,
}

fn bucket(raw: RawStats, admin_ids: &[i64], months: &[Month]) -> HashMap<Month, MonthData> {
    let mut data: HashMap<Month, MonthData> =
        months.iter().map(|&m| (m, MonthData::default())).collect();
    for b in without_admin_bookings(raw.bookings, admin_ids) {
        let Some(d) = Month::of_timestamp(b.start_time.timestamp()).and_then(|m| data.get_mut(&m))
        else {
            continue;
        };
        d.booked_seconds += (b.end_time - b.start_time).num_seconds().max(0);
        d.users.insert(b.user_id);
        if let Some(c) = &b.country {
            d.user_countries.insert((b.user_id, c.clone()));
        }
        d.bookings.push(b);
    }
    for g in raw.guests {
        let Some(d) = Month::of_timestamp(g.started_at).and_then(|m| data.get_mut(&m)) else {
            continue;
        };
        d.guest_sessions += 1;
        // Only finished sessions have a known length. Capped at the hard
        // ceiling: a session left open by a server restart is only ended
        // when the monitor next runs, which can be long after its guest left.
        if let Some(end) = g.ended_at {
            d.guest_seconds += (end - g.started_at).clamp(0, GUEST_SESSION_HARD_CEILING_SECS);
        }
        if let Some(c) = g.country {
            *d.guest_countries.entry(c).or_default() += 1;
        }
    }
    for o in raw.observations {
        if admin_ids.contains(&o.user_id) {
            continue;
        }
        if let Some(d) = Month::of_timestamp(o.start_time).and_then(|m| data.get_mut(&m)) {
            d.observations += 1;
        }
    }
    data
}

/// Figures for a set of months, ready to print.
struct Figures {
    observations: usize,
    bookings: usize,
    booked_seconds: i64,
    days: i64,
    users: usize,
    guest_sessions: usize,
    guest_seconds: i64,
}

impl Figures {
    fn of(months: &[Month], data: &HashMap<Month, MonthData>) -> Figures {
        let mut f = Figures {
            observations: 0,
            bookings: 0,
            booked_seconds: 0,
            days: 0,
            users: 0,
            guest_sessions: 0,
            guest_seconds: 0,
        };
        let mut users: HashSet<i64> = HashSet::new();
        for m in months {
            let d = &data[m];
            f.observations += d.observations;
            f.bookings += count_booking_segments(&d.bookings);
            f.booked_seconds += d.booked_seconds;
            f.days += m.days();
            users.extend(&d.users);
            f.guest_sessions += d.guest_sessions;
            f.guest_seconds += d.guest_seconds;
        }
        f.users = users.len();
        f
    }

    fn hours(&self) -> i64 {
        (self.booked_seconds as f64 / 3600.0).round() as i64
    }

    fn hours_per_week(&self) -> f64 {
        if self.days == 0 {
            return 0.0;
        }
        self.booked_seconds as f64 / 3600.0 / (self.days as f64 / 7.0)
    }
}

fn fmt_int(n: i64, lang: Language) -> String {
    let sep = match lang {
        Language::English => ",",
        Language::Swedish => "\u{a0}",
    };
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn fmt_dec(x: f64, lang: Language) -> String {
    let s = format!("{x:.1}");
    match lang {
        Language::English => s,
        Language::Swedish => s.replace('.', ","),
    }
}

struct Row {
    label: String,
    observations: String,
    bookings: String,
    hours: String,
    hours_per_week: String,
    users: String,
    guest_sessions: String,
    guest_hours: String,
}

impl Row {
    fn new(label: String, f: &Figures, lang: Language) -> Row {
        Row {
            label,
            observations: fmt_int(f.observations as i64, lang),
            bookings: fmt_int(f.bookings as i64, lang),
            hours: fmt_int(f.hours(), lang),
            hours_per_week: fmt_dec(f.hours_per_week(), lang),
            users: fmt_int(f.users as i64, lang),
            guest_sessions: fmt_int(f.guest_sessions as i64, lang),
            guest_hours: fmt_dec(f.guest_seconds as f64 / 3600.0, lang),
        }
    }
}

struct Bar {
    label: String,
    tip: String,
    height_pct: String,
}

/// A labelled gridline on the chart's hour scale.
struct Tick {
    label: String,
    bottom_pct: String,
}

/// A round step (1, 2 or 5 times a power of ten) giving about three
/// gridlines up to `max`.
fn nice_step(max: f64) -> f64 {
    let rough = (max / 3.0).max(1.0);
    let magnitude = 10f64.powf(rough.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|f| f * magnitude)
        .find(|&s| s >= rough)
        .unwrap_or(10.0 * magnitude)
}

struct CountryRow {
    /// ISO 3166 code; the page script swaps in the localized name.
    code: String,
    count: String,
    width_pct: String,
}

struct CountryList {
    top: Vec<CountryRow>,
    other: Vec<CountryRow>,
    other_total: String,
    other_width_pct: String,
    /// "34 countries", for the card subtitle.
    country_count: String,
}

fn country_list(counts: HashMap<String, usize>, lang: Language) -> CountryList {
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let max = rows.first().map_or(1, |r| r.1).max(1) as f64;
    let width = |n: usize| format!("{:.1}", (n as f64 / max * 100.0).min(100.0));
    let country_count = match rows.len() {
        1 => lang.t("stats-one-country"),
        n => lang
            .t("stats-n-countries")
            .replace("%n%", &fmt_int(n as i64, lang)),
    };
    let other_total: usize = rows.iter().skip(TOP_COUNTRIES).map(|r| r.1).sum();
    let to_row = |(code, n): (String, usize)| CountryRow {
        code,
        count: fmt_int(n as i64, lang),
        width_pct: width(n),
    };
    let other = rows.split_off(rows.len().min(TOP_COUNTRIES));
    CountryList {
        top: rows.into_iter().map(to_row).collect(),
        other: other.into_iter().map(to_row).collect(),
        other_total: fmt_int(other_total as i64, lang),
        other_width_pct: width(other_total),
        country_count,
    }
}

struct Shortcut {
    label: String,
    href: String,
    active: bool,
    /// Off while fewer months exist than the shortcut names, so it never
    /// shows fewer months than its label says.
    enabled: bool,
    disabled_title: String,
}

struct MonthOption {
    value: String,
    label: String,
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsTemplate {
    lang: Language,
    has_data: bool,
    // Since the start
    registered_users: String,
    total_hours: String,
    total_hours_per_week: String,
    total_guest_sessions: String,
    total_guest_hours: String,
    total_observations: String,
    // Period picker
    shortcuts: Vec<Shortcut>,
    month_options: Vec<MonthOption>,
    from: String,
    to: String,
    range_label: String,
    // Selected period
    bars: Vec<Bar>,
    ticks: Vec<Tick>,
    /// More than a year of bars: only years are labelled.
    long_chart: bool,
    user_countries: CountryList,
    guest_countries: CountryList,
    rows: Vec<Row>,
    total: Option<Row>,
}

#[derive(Deserialize)]
struct StatsQuery {
    from: Option<String>,
    to: Option<String>,
}

async fn get_stats(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    State(state): State<AppState>,
    Query(query): Query<StatsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let months = completed_months(Utc::now().date_naive());
    let raw = fetch(
        state.database_connection.clone(),
        FIRST_MONTH.start_timestamp(),
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let admin_ids = &state.admin_config.user_ids;
    let registered_users = raw
        .registered_user_ids
        .iter()
        .filter(|id| !admin_ids.contains(id))
        .count();
    let data = bucket(raw, admin_ids, &months);

    let content = render(lang, &months, &data, registered_users, &query);
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Ok(Html(content))
}

/// The selected range as indices into `months`, clamped to what exists and
/// put in order. Defaults to everything.
fn selected_range(months: &[Month], query: &StatsQuery) -> (usize, usize) {
    let last = months.len().saturating_sub(1);
    let index = |s: &Option<String>, default: usize| {
        s.as_deref()
            .and_then(Month::parse)
            .map(|m| months.partition_point(|x| *x < m).min(last))
            .unwrap_or(default)
    };
    let (a, b) = (index(&query.from, 0), index(&query.to, last));
    (a.min(b), a.max(b))
}

fn render(
    lang: Language,
    months: &[Month],
    data: &HashMap<Month, MonthData>,
    registered_users: usize,
    query: &StatsQuery,
) -> String {
    let all = Figures::of(months, data);
    let (a, b) = selected_range(months, query);
    let selected: &[Month] = if months.is_empty() {
        &[]
    } else {
        &months[a..=b]
    };

    let link = |from: Month, to: Month| format!("/stats?from={}&to={}", from.param(), to.param());
    let shortcuts = [
        (Some(1), "stats-last-month"),
        (Some(6), "stats-last-6-months"),
        (Some(12), "stats-last-12-months"),
        (None, "stats-since-start"),
    ]
    .into_iter()
    .map(|(n, key)| {
        let enabled = n.is_none_or(|n| n <= months.len());
        let start = n.map_or(0, |n| months.len().saturating_sub(n));
        Shortcut {
            label: lang.t(key),
            href: match (months.get(start), months.last()) {
                (Some(&f), Some(&t)) => link(f, t),
                _ => "/stats".to_string(),
            },
            active: enabled && !months.is_empty() && a == start && b == months.len() - 1,
            enabled,
            disabled_title: lang
                .t("stats-shortcut-unavailable")
                .replace("%n%", &n.unwrap_or(0).to_string()),
        }
    })
    .collect();

    let month_options = months
        .iter()
        .map(|m| MonthOption {
            value: m.param(),
            label: m.format("%B %Y", lang),
        })
        .collect();

    let range_label = match selected {
        [] => String::new(),
        [only] => format!(
            "{} · 1 {}",
            only.format("%B %Y", lang),
            lang.t("stats-month")
        ),
        [first, .., last] => format!(
            "{} – {} · {} {}",
            first.format("%B %Y", lang),
            last.format("%B %Y", lang),
            selected.len(),
            lang.t("stats-months")
        ),
    };

    // The scale tops out at the first round number at or above the busiest
    // month, so bars are drawn against the same hours the axis labels show.
    let max_hours = selected
        .iter()
        .map(|m| data[m].booked_seconds)
        .max()
        .unwrap_or(0) as f64
        / 3600.0;
    let step = nice_step(max_hours);
    let top = (max_hours / step).ceil().max(1.0) * step;
    let tick_count = (top / step).round() as i64;
    let ticks = (0..=tick_count)
        .map(|i| {
            let value = step * i as f64;
            let mut label = fmt_int(value.round() as i64, lang);
            if i == tick_count {
                label.push_str(" h");
            }
            Tick {
                label,
                bottom_pct: format!("{:.1}", value / top * 100.0),
            }
        })
        .collect();
    let bars = selected
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let f = Figures::of(&[*m], data);
            Bar {
                label: bar_label(*m, i, selected.len(), lang),
                tip: format!(
                    "{}: {} h · {} h / {}",
                    m.format("%B %Y", lang),
                    fmt_int(f.hours(), lang),
                    fmt_dec(f.hours_per_week(), lang),
                    lang.t("stats-week")
                ),
                height_pct: format!(
                    "{:.1}",
                    data[m].booked_seconds as f64 / 3600.0 / top * 100.0
                ),
            }
        })
        .collect();

    let mut user_country_pairs = HashSet::new();
    let mut guest_counts: HashMap<String, usize> = HashMap::new();
    for m in selected {
        user_country_pairs.extend(data[m].user_countries.iter().cloned());
        for (c, n) in &data[m].guest_countries {
            *guest_counts.entry(c.clone()).or_default() += n;
        }
    }
    let mut user_counts: HashMap<String, usize> = HashMap::new();
    for (_, c) in user_country_pairs {
        *user_counts.entry(c).or_default() += 1;
    }

    let rows = selected
        .iter()
        .map(|m| Row::new(m.format("%B %Y", lang), &Figures::of(&[*m], data), lang))
        .collect();
    let total = (selected.len() > 1)
        .then(|| Row::new(lang.t("stats-total"), &Figures::of(selected, data), lang));

    StatsTemplate {
        lang,
        has_data: !months.is_empty(),
        registered_users: fmt_int(registered_users as i64, lang),
        total_hours: fmt_int(all.hours(), lang),
        total_hours_per_week: fmt_dec(all.hours_per_week(), lang),
        total_guest_sessions: fmt_int(all.guest_sessions as i64, lang),
        total_guest_hours: fmt_dec(all.guest_seconds as f64 / 3600.0, lang),
        total_observations: fmt_int(all.observations as i64, lang),
        shortcuts,
        month_options,
        from: selected.first().map(|m| m.param()).unwrap_or_default(),
        to: selected.last().map(|m| m.param()).unwrap_or_default(),
        range_label,
        long_chart: selected.len() > 12,
        bars,
        ticks,
        user_countries: country_list(user_counts, lang),
        guest_countries: country_list(guest_counts, lang),
        rows,
        total,
    }
    .render()
    .expect("Template rendering should always succeed")
}

#[cfg(test)]
mod test {
    use super::*;

    fn month(year: i32, month: u32) -> Month {
        Month { year, month }
    }

    #[test]
    fn completed_months_excludes_the_current_month() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert_eq!(
            completed_months(today),
            vec![month(2026, 5), month(2026, 6), month(2026, 7)]
        );
    }

    #[test]
    fn completed_months_is_empty_in_the_launch_month() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 31).unwrap();
        assert!(completed_months(today).is_empty());
    }

    #[test]
    fn month_rolls_over_the_year() {
        assert_eq!(month(2026, 12).next(), month(2027, 1));
        assert_eq!(month(2027, 2).days(), 28);
    }

    #[test]
    fn parse_rejects_nonsense() {
        assert_eq!(Month::parse("2026-05"), Some(month(2026, 5)));
        assert_eq!(Month::parse("2026-13"), None);
        assert_eq!(Month::parse("may"), None);
    }

    #[test]
    fn range_is_clamped_and_ordered() {
        let months = completed_months(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        let q = |from: &str, to: &str| StatsQuery {
            from: Some(from.into()),
            to: Some(to.into()),
        };
        // May..Sep 2026 → indices 0..4
        assert_eq!(selected_range(&months, &q("2026-07", "2026-06")), (1, 2));
        assert_eq!(selected_range(&months, &q("2020-01", "2030-01")), (0, 4));
        assert_eq!(
            selected_range(
                &months,
                &StatsQuery {
                    from: None,
                    to: None
                }
            ),
            (0, 4)
        );
    }

    #[test]
    fn scale_steps_are_round_numbers() {
        assert_eq!(nice_step(230.0), 100.0);
        assert_eq!(nice_step(140.0), 50.0);
        assert_eq!(nice_step(40.0), 20.0);
        assert_eq!(nice_step(9.0), 5.0);
        // An empty period still gets a usable scale.
        assert_eq!(nice_step(0.0), 1.0);
    }

    #[test]
    fn long_ranges_label_only_years() {
        let en = Language::English;
        assert_eq!(bar_label(month(2026, 9), 0, 12, en), "Sep");
        assert_eq!(bar_label(month(2026, 5), 0, 30, en), "May 2026");
        assert_eq!(bar_label(month(2026, 6), 1, 30, en), "");
        assert_eq!(bar_label(month(2027, 1), 8, 30, en), "2027");
        // Too close to the next January to fit a label of its own.
        assert_eq!(bar_label(month(2026, 11), 0, 30, en), "");
    }

    #[test]
    fn thousands_are_grouped_per_language() {
        assert_eq!(fmt_int(1234567, Language::English), "1,234,567");
        assert_eq!(fmt_int(1234, Language::Swedish), "1\u{a0}234");
        assert_eq!(fmt_int(999, Language::English), "999");
        assert_eq!(fmt_dec(42.25, Language::Swedish), "42,2");
    }

    #[test]
    fn countries_beyond_the_top_ten_fold_into_other() {
        let counts: HashMap<String, usize> =
            (0..12).map(|i| (format!("C{i:02}"), 20 - i)).collect();
        let list = country_list(counts, Language::English);
        assert_eq!(list.top.len(), 10);
        assert_eq!(list.other.len(), 2);
        // C10 has 10, C11 has 9.
        assert_eq!(list.other_total, "19");
    }

    #[test]
    fn admin_bookings_and_observations_are_left_out() {
        let at = |m: Month, hour: i64| m.start_timestamp() + hour * 3600;
        let may = month(2026, 5);
        let booking = |user_id: i64, start: i64| Booking {
            id: 0,
            start_time: chrono::DateTime::from_timestamp(start, 0).unwrap(),
            end_time: chrono::DateTime::from_timestamp(start + 3600, 0).unwrap(),
            telescope_name: "vale".into(),
            user_id,
            user_name: String::new(),
            user_provider: String::new(),
            description: None,
            country: Some("SE".into()),
        };
        let raw = RawStats {
            bookings: vec![booking(1, at(may, 0)), booking(2, at(may, 5))],
            guests: vec![],
            observations: vec![
                crate::models::stats::ObservationRow {
                    user_id: 1,
                    start_time: at(may, 0),
                },
                crate::models::stats::ObservationRow {
                    user_id: 2,
                    start_time: at(may, 5),
                },
            ],
            registered_user_ids: vec![1, 2],
        };
        let data = bucket(raw, &[2], &[may]);
        let f = Figures::of(&[may], &data);
        assert_eq!(f.bookings, 1);
        assert_eq!(f.observations, 1);
        assert_eq!(f.users, 1);
        assert_eq!(f.hours(), 1);
    }
}
