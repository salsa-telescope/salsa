//! Target-visibility planner: given a coordinate (galactic / equatorial /
//! Sun) and a date, plot the target's elevation across the day at the
//! SALSA site so users can pick a booking window where the target is
//! actually above the horizon.
//!
//! Linked from the support page but not part of "support" — it's a
//! planning tool, kept at the top-level `/visibility` URL.

use std::fmt::Write;

use askama::Template;
use axum::{
    Extension, Router,
    extract::Query,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::get,
};
use chrono::{DateTime, Duration, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Deserialize;

use crate::coords::{
    Direction, ONSALA_LOCATION, PRACTICAL_ELEVATION_LIMIT_DEG, horizontal_from_equatorial,
    horizontal_from_galactic, horizontal_from_sun,
};
use crate::i18n::Language;
use crate::models::user::User;
use crate::routes::index::render_main;
use i18n_embed_fl::fl;

const VISIBILITY_THRESHOLD_DEG: f64 = PRACTICAL_ELEVATION_LIMIT_DEG;
// Sample every 10 minutes across the day. A normal day gives 145 points
// (0:00 inclusive ... 24:00 inclusive); DST transition days a few more or
// fewer.
const SAMPLE_STEP_MIN: i64 = 10;

pub fn routes() -> Router {
    Router::new().route("/", get(get_visibility))
}

#[derive(Deserialize, Default)]
struct VisibilityForm {
    coord: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    date: Option<String>,
}

struct VisibilityResult {
    svg: String,
}

#[derive(Template)]
#[template(path = "visibility.html")]
struct VisibilityTemplate {
    lang: Language,
    coord: String,
    x: String,
    y: String,
    date: String,
    tz_name: String,
    error: Option<String>,
    result: Option<VisibilityResult>,
}

async fn get_visibility(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    Query(form): Query<VisibilityForm>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Logged-in users see the chart in their profile timezone (UTC until
    // they've picked one); guests always get UTC.
    let tz = user.as_ref().map(|u| u.tz()).unwrap_or(chrono_tz::UTC);
    let coord = form.coord.as_deref().unwrap_or("galactic").to_string();
    // Match the observe page's idle-telescope default (glon=140, glat=0):
    // a bright HI-line target in the Galactic disk that's a sensible
    // first-look without the user having to pick coordinates.
    let x_val = form.x.unwrap_or(140.0);
    let y_val = form.y.unwrap_or(0.0);
    let today = Utc::now()
        .with_timezone(&tz)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let date_str = form
        .date
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&today)
        .to_string();

    let mut error: Option<String> = None;
    let mut result: Option<VisibilityResult> = None;
    match (
        NaiveDate::parse_from_str(&date_str, "%Y-%m-%d"),
        coord.as_str(),
    ) {
        (Err(_), _) => error = Some(fl!(lang.loader(), "vis-error-date")),
        (Ok(date), c @ ("galactic" | "equatorial" | "sun")) => {
            result = Some(compute_visibility(c, x_val, y_val, date, tz, lang));
        }
        (Ok(_), _) => error = Some(fl!(lang.loader(), "vis-error-coord")),
    }

    let template = VisibilityTemplate {
        lang,
        coord,
        x: format!("{x_val}"),
        y: format!("{y_val}"),
        date: date_str,
        tz_name: tz.name().to_string(),
        error,
        result,
    };
    let content = template
        .render()
        .unwrap_or_else(|_| "<p>Visibility page failed to render.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, lang, content)
    };
    Html(content)
}

/// UTC instant where `date` begins in `tz`, and the day's length in
/// minutes. Normally midnight-to-midnight (1440 min), but DST transition
/// days are 23 h or 25 h, and in zones that spring forward over midnight
/// the day starts at the first instant that exists after the gap.
fn local_day_span(tz: Tz, date: NaiveDate) -> (DateTime<Utc>, i64) {
    let start = local_day_start(tz, date);
    let day_len_min = date
        .succ_opt()
        .map(|next| (local_day_start(tz, next) - start).num_minutes())
        .filter(|len| *len > 0)
        .unwrap_or(24 * 60);
    (start, day_len_min)
}

fn local_day_start(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .expect("00:00:00 is always a valid time");
    match tz.from_local_datetime(&midnight) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        // Midnight falls in a DST gap (e.g. America/Santiago): the day
        // starts an hour later. Gaps longer than 1 h don't occur in tzdb;
        // fall back to plain UTC midnight if the retry somehow misses too.
        LocalResult::None => (midnight + Duration::hours(1))
            .and_local_timezone(tz)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&midnight)),
    }
}

fn compute_visibility(
    coord: &str,
    x_deg: f64,
    y_deg: f64,
    date: NaiveDate,
    tz: Tz,
    lang: Language,
) -> VisibilityResult {
    let location = ONSALA_LOCATION;
    let x_rad = x_deg.to_radians();
    let y_rad = y_deg.to_radians();
    let (day_start, day_len_min) = local_day_span(tz, date);
    // The end-of-day sample is the next local midnight; show it as 24:00
    // rather than a confusing 00:00 at the right edge of the chart.
    let fmt_local = |minutes: i64| {
        if minutes == day_len_min {
            "24:00".to_string()
        } else {
            (day_start + Duration::minutes(minutes))
                .with_timezone(&tz)
                .format("%H:%M")
                .to_string()
        }
    };

    let n_steps = day_len_min / SAMPLE_STEP_MIN;
    let mut samples: Vec<(i64, f64)> = Vec::with_capacity((n_steps + 1) as usize);
    // All contiguous windows where elevation >= threshold. The sidereal day is
    // ~4 minutes shorter than 24h, so some targets rise twice in one UTC day —
    // we need to collect every window, not just the first-above / last-above
    // pair (which would falsely report a single mega-window covering the gap).
    let mut windows: Vec<(i64, i64)> = Vec::new();
    let mut current_start: Option<i64> = None;
    let mut last_above_min: i64 = 0;
    let mut max_el = f64::NEG_INFINITY;
    let mut max_at: i64 = 0;

    for step in 0..=n_steps {
        let minutes = step * SAMPLE_STEP_MIN;
        let when = day_start + Duration::minutes(minutes);
        let dir: Direction = match coord {
            "galactic" => horizontal_from_galactic(location, when, x_rad, y_rad),
            "equatorial" => horizontal_from_equatorial(location, when, x_rad, y_rad),
            "sun" => horizontal_from_sun(location, when),
            _ => Direction {
                azimuth: 0.0,
                elevation: 0.0,
            },
        };
        let el = dir.elevation.to_degrees();
        samples.push((minutes, el));
        if el >= VISIBILITY_THRESHOLD_DEG {
            if current_start.is_none() {
                current_start = Some(minutes);
            }
            last_above_min = minutes;
        } else if let Some(start) = current_start.take() {
            windows.push((start, last_above_min));
        }
        if el > max_el {
            max_el = el;
            max_at = minutes;
        }
    }
    if let Some(start) = current_start {
        windows.push((start, last_above_min));
    }

    let target_label = match coord {
        "galactic" => {
            let x = format!("{x_deg}");
            let y = format!("{y_deg}");
            fl!(lang.loader(), "vis-target-galactic", x = x, y = y)
        }
        "equatorial" => {
            let x = format!("{x_deg}");
            let y = format!("{y_deg}");
            fl!(lang.loader(), "vis-target-equatorial", x = x, y = y)
        }
        "sun" => fl!(lang.loader(), "observe-coord-sun"),
        _ => coord.to_string(),
    };
    let title_line1 = fl!(
        lang.loader(),
        "vis-title",
        target = target_label,
        date = format!("{date}"),
        tz = tz.name()
    );
    let threshold = format!("{VISIBILITY_THRESHOLD_DEG:.0}");
    let max = format!("{max_el:.1}");
    let peak = fmt_local(max_at);
    let title_line2 = if windows.is_empty() {
        fl!(
            lang.loader(),
            "vis-not-above",
            threshold = threshold,
            max = max,
            peak = peak
        )
    } else {
        let windows_str = windows
            .iter()
            .map(|(s, e)| {
                let from = fmt_local(*s);
                let to = fmt_local(*e);
                fl!(lang.loader(), "vis-window-range", from = from, to = to)
            })
            .collect::<Vec<_>>()
            .join(&fl!(lang.loader(), "vis-window-join"));
        fl!(
            lang.loader(),
            "vis-above",
            threshold = threshold,
            windows = windows_str,
            max = max,
            peak = peak
        )
    };
    let axis_label = if tz == chrono_tz::UTC {
        fl!(lang.loader(), "vis-axis-utc")
    } else {
        fl!(lang.loader(), "vis-axis-local", tz = tz.name())
    };

    VisibilityResult {
        svg: build_svg(
            &samples,
            day_len_min,
            &fmt_local,
            &title_line1,
            &title_line2,
            &axis_label,
        ),
    }
}

fn build_svg(
    samples: &[(i64, f64)],
    day_len_min: i64,
    fmt_local: &dyn Fn(i64) -> String,
    title_line1: &str,
    title_line2: &str,
    axis_label: &str,
) -> String {
    let width = 720.0_f64;
    let height = 360.0_f64;
    let m_left = 60.0_f64;
    let m_right = 20.0_f64;
    // Top margin holds two title lines (line 1 ~y=20, line 2 ~y=38) plus a
    // little breathing room before the plot area.
    let m_top = 56.0_f64;
    let m_bottom = 40.0_f64;
    let plot_w = width - m_left - m_right;
    let plot_h = height - m_top - m_bottom;
    let plot_right = m_left + plot_w;
    let plot_bottom = m_top + plot_h;

    let x_for = |minutes: i64| m_left + (minutes as f64 / day_len_min as f64) * plot_w;
    let y_for = |el: f64| m_top + (90.0 - el.clamp(0.0, 90.0)) / 90.0 * plot_h;

    let mut path = String::new();
    for (i, (minutes, el)) in samples.iter().enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        let _ = write!(path, "{} {:.2} {:.2} ", cmd, x_for(*minutes), y_for(*el));
    }

    // Per-sample "HH:MM,elevation" pairs read by the crosshair script in
    // visibility.html. Samples are uniformly spaced, so together with the
    // plot geometry (also passed as data attributes) the script can map a
    // pointer position to the nearest sample without knowing about time.
    let mut cursor_data = String::new();
    for (i, (minutes, el)) in samples.iter().enumerate() {
        let sep = if i == 0 { "" } else { ";" };
        let _ = write!(cursor_data, "{sep}{},{el:.1}", fmt_local(*minutes));
    }

    let mut x_ticks = String::new();
    // Wall-clock labels every 3 h plus one at the end of the day. On DST
    // days the elapsed-minute axis makes hour labels jump (e.g. 00:00,
    // 04:00, 07:00 after springing forward) — intentional, the axis is
    // elapsed time.
    let mut tick_min = 0;
    while tick_min <= day_len_min {
        let label = fmt_local(tick_min);
        let xt = x_for(tick_min);
        let y_tick_top = plot_bottom;
        let y_tick_bot = y_tick_top + 4.0;
        let y_label = y_tick_bot + 12.0;
        let _ = write!(
            x_ticks,
            r##"<line x1="{xt:.2}" y1="{y_tick_top:.2}" x2="{xt:.2}" y2="{y_tick_bot:.2}" stroke="#9ca3af"/><text x="{xt:.2}" y="{y_label:.2}" text-anchor="middle" font-size="11" fill="#4b5563">{label}</text>"##,
        );
        tick_min = if tick_min < day_len_min && tick_min + 180 > day_len_min {
            day_len_min
        } else {
            tick_min + 180
        };
    }
    let mut y_ticks = String::new();
    for el in [0_i32, 30, 60, 90] {
        let yt = y_for(el as f64);
        let x_tick_left = m_left - 4.0;
        let x_label = m_left - 8.0;
        let _ = write!(
            y_ticks,
            r##"<line x1="{x_tick_left:.2}" y1="{yt:.2}" x2="{m_left:.2}" y2="{yt:.2}" stroke="#9ca3af"/><text x="{x_label:.2}" y="{yt:.2}" text-anchor="end" alignment-baseline="middle" font-size="11" fill="#4b5563">{el}°</text>"##,
        );
    }
    let y_thresh = y_for(VISIBILITY_THRESHOLD_DEG);
    let x_label_pos = m_left + plot_w / 2.0;
    let x_label_y = height - 4.0;
    let y_label_pos_x = 14.0_f64;
    let y_label_pos_y = m_top + plot_h / 2.0;

    let title_x = width / 2.0;
    let title_l1 = escape_xml(title_line1);
    let title_l2 = escape_xml(title_line2);
    let axis_l = escape_xml(axis_label);

    format!(
        r##"<svg viewBox="0 0 {width:.0} {height:.0}" xmlns="http://www.w3.org/2000/svg" style="max-width:100%;height:auto;" data-samples="{cursor_data}" data-mleft="{m_left:.2}" data-mtop="{m_top:.2}" data-plotw="{plot_w:.2}" data-ploth="{plot_h:.2}">
  <text x="{title_x:.2}" y="22" text-anchor="middle" font-size="13" font-weight="600" fill="#111827">{title_l1}</text>
  <text x="{title_x:.2}" y="42" text-anchor="middle" font-size="12" fill="#4b5563">{title_l2}</text>
  <rect x="{m_left:.2}" y="{m_top:.2}" width="{plot_w:.2}" height="{plot_h:.2}" fill="#f9fafb" stroke="none"/>
  <line x1="{m_left:.2}" y1="{m_top:.2}" x2="{m_left:.2}" y2="{plot_bottom:.2}" stroke="#9ca3af"/>
  <line x1="{m_left:.2}" y1="{plot_bottom:.2}" x2="{plot_right:.2}" y2="{plot_bottom:.2}" stroke="#9ca3af"/>
  {x_ticks}
  {y_ticks}
  <line x1="{m_left:.2}" y1="{y_thresh:.2}" x2="{plot_right:.2}" y2="{y_thresh:.2}" stroke="#f59e0b" stroke-width="1.5" stroke-dasharray="4,3"/>
  <text x="{plot_right:.2}" y="{y_thresh:.2}" dx="-4" dy="-4" text-anchor="end" font-size="11" fill="#b45309">{threshold:.0}° threshold</text>
  <path d="{path}" fill="none" stroke="#1d4ed8" stroke-width="1.5"/>
  <text x="{x_label_pos:.2}" y="{x_label_y:.2}" text-anchor="middle" font-size="12" fill="#374151">{axis_l}</text>
  <text x="{y_label_pos_x:.2}" y="{y_label_pos_y:.2}" text-anchor="middle" font-size="12" fill="#374151" transform="rotate(-90 {y_label_pos_x:.2} {y_label_pos_y:.2})">Elevation</text>
  <g class="vis-cursor" visibility="hidden" pointer-events="none">
    <line class="vis-cursor-line" y1="{m_top:.2}" y2="{plot_bottom:.2}" stroke="#6b7280" stroke-dasharray="3,3"/>
    <circle class="vis-cursor-dot" r="3.5" fill="#1d4ed8"/>
    <text class="vis-cursor-text" y="{cursor_text_y:.2}" font-size="11" font-weight="600" fill="#111827"/>
  </g>
  <rect class="vis-capture" x="{m_left:.2}" y="{m_top:.2}" width="{plot_w:.2}" height="{plot_h:.2}" fill="transparent"/>
</svg>"##,
        threshold = VISIBILITY_THRESHOLD_DEG,
        cursor_text_y = m_top + 14.0,
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn utc_day_is_24_hours_from_utc_midnight() {
        let (start, len) = local_day_span(chrono_tz::UTC, date("2026-07-12"));
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 7, 12, 0, 0, 0).unwrap());
        assert_eq!(len, 1440);
    }

    #[test]
    fn stockholm_day_starts_at_local_midnight() {
        // CEST = UTC+2 in July, so the local day starts at 22:00 UTC the
        // evening before.
        let (start, len) = local_day_span(chrono_tz::Europe::Stockholm, date("2026-07-12"));
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 7, 11, 22, 0, 0).unwrap());
        assert_eq!(len, 1440);
    }

    #[test]
    fn dst_transition_days_are_23_and_25_hours() {
        let tz = chrono_tz::Europe::Stockholm;
        let (_, spring) = local_day_span(tz, date("2026-03-29"));
        assert_eq!(spring, 23 * 60);
        let (_, fall) = local_day_span(tz, date("2026-10-25"));
        assert_eq!(fall, 25 * 60);
    }

    #[test]
    fn midnight_dst_gap_starts_day_after_the_gap() {
        // Chile springs forward at 2026-09-06 00:00 -> 01:00, so local
        // midnight doesn't exist and the 23-hour day starts at 01:00.
        let tz = chrono_tz::America::Santiago;
        let (start, len) = local_day_span(tz, date("2026-09-06"));
        assert_eq!(
            start.with_timezone(&tz).format("%H:%M").to_string(),
            "01:00"
        );
        assert_eq!(len, 23 * 60);
    }
}
