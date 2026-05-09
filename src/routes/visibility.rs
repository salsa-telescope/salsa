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
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use crate::coords::{
    Direction, Location, horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun,
};
use crate::models::user::User;
use crate::routes::index::render_main;

// SALSA telescopes are co-located at Onsala Space Observatory; the same
// values are used as defaults in config.toml.example. The visibility planner
// always uses this site (telescopes are too close to differ on visibility).
const ONSALA_LON_DEG: f64 = 11.9188;
const ONSALA_LAT_DEG: f64 = 57.3934;
const VISIBILITY_THRESHOLD_DEG: f64 = 10.0;
// Sample every 10 minutes across the day -> 145 points (0:00 inclusive ... 24:00 inclusive).
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
#[template(path = "visibility.html", escape = "none")]
struct VisibilityTemplate {
    coord: String,
    x: String,
    y: String,
    date: String,
    error: Option<String>,
    result: Option<VisibilityResult>,
}

async fn get_visibility(
    Extension(user): Extension<Option<User>>,
    Query(form): Query<VisibilityForm>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let coord = form.coord.as_deref().unwrap_or("galactic").to_string();
    let x_val = form.x.unwrap_or(0.0);
    let y_val = form.y.unwrap_or(0.0);
    let today = Utc::now().date_naive().format("%Y-%m-%d").to_string();
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
        (Err(_), _) => error = Some("Invalid date — use YYYY-MM-DD.".to_string()),
        (Ok(date), c @ ("galactic" | "equatorial" | "sun")) => {
            result = Some(compute_visibility(c, x_val, y_val, date));
        }
        (Ok(_), _) => error = Some("Invalid coordinate system.".to_string()),
    }

    let template = VisibilityTemplate {
        coord,
        x: format!("{x_val}"),
        y: format!("{y_val}"),
        date: date_str,
        error,
        result,
    };
    let content = template
        .render()
        .unwrap_or_else(|_| "<p>Visibility page failed to render.</p>".to_string());
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(user, content)
    };
    Html(content)
}

fn compute_visibility(coord: &str, x_deg: f64, y_deg: f64, date: NaiveDate) -> VisibilityResult {
    let location = Location {
        longitude: ONSALA_LON_DEG.to_radians(),
        latitude: ONSALA_LAT_DEG.to_radians(),
    };
    let x_rad = x_deg.to_radians();
    let y_rad = y_deg.to_radians();
    let day_start = Utc.from_utc_datetime(
        &date
            .and_hms_opt(0, 0, 0)
            .expect("00:00:00 is always a valid time"),
    );

    let n_steps = 24 * 60 / SAMPLE_STEP_MIN;
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
        "galactic" => format!("Galactic {x_deg}°, {y_deg}°"),
        "equatorial" => format!("Equatorial {x_deg}°, {y_deg}°"),
        "sun" => "Sun".to_string(),
        _ => coord.to_string(),
    };
    let title_line1 = format!("{target_label} on {date} (UTC)");
    let title_line2 = if windows.is_empty() {
        format!(
            "Not above {threshold:.0}° at any time. Peak {max:.1}° at {peak}.",
            threshold = VISIBILITY_THRESHOLD_DEG,
            max = max_el,
            peak = fmt_hhmm(max_at),
        )
    } else {
        let windows_str = windows
            .iter()
            .map(|(s, e)| format!("{} to {}", fmt_hhmm(*s), fmt_hhmm(*e)))
            .collect::<Vec<_>>()
            .join(", and ");
        format!(
            "Above {threshold:.0}° from {windows_str}. Max {max:.1}° at {peak}.",
            threshold = VISIBILITY_THRESHOLD_DEG,
            max = max_el,
            peak = fmt_hhmm(max_at),
        )
    };

    VisibilityResult {
        svg: build_svg(&samples, &title_line1, &title_line2),
    }
}

fn fmt_hhmm(minutes: i64) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn build_svg(samples: &[(i64, f64)], title_line1: &str, title_line2: &str) -> String {
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

    let x_for = |minutes: i64| m_left + (minutes as f64 / 1440.0) * plot_w;
    let y_for = |el: f64| m_top + (90.0 - el.clamp(0.0, 90.0)) / 90.0 * plot_h;

    let mut path = String::new();
    for (i, (minutes, el)) in samples.iter().enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        let _ = write!(path, "{} {:.2} {:.2} ", cmd, x_for(*minutes), y_for(*el));
    }

    let mut x_ticks = String::new();
    for h in (0..=24).step_by(3) {
        let xt = x_for((h as i64) * 60);
        let y_tick_top = plot_bottom;
        let y_tick_bot = y_tick_top + 4.0;
        let y_label = y_tick_bot + 12.0;
        let _ = write!(
            x_ticks,
            r##"<line x1="{xt:.2}" y1="{y_tick_top:.2}" x2="{xt:.2}" y2="{y_tick_bot:.2}" stroke="#9ca3af"/><text x="{xt:.2}" y="{y_label:.2}" text-anchor="middle" font-size="11" fill="#4b5563">{h:02}:00</text>"##,
        );
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

    format!(
        r##"<svg viewBox="0 0 {width:.0} {height:.0}" xmlns="http://www.w3.org/2000/svg" style="max-width:100%;height:auto;">
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
  <text x="{x_label_pos:.2}" y="{x_label_y:.2}" text-anchor="middle" font-size="12" fill="#374151">UTC time</text>
  <text x="{y_label_pos_x:.2}" y="{y_label_pos_y:.2}" text-anchor="middle" font-size="12" fill="#374151" transform="rotate(-90 {y_label_pos_x:.2} {y_label_pos_y:.2})">Elevation</text>
</svg>"##,
        threshold = VISIBILITY_THRESHOLD_DEG,
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
