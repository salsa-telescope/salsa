use crate::app::AppState;
use crate::fits::{SpectrumMeta, write_spectrum_fits};
use crate::i18n::Language;
use crate::models::interferometry::InterferometrySession;
use crate::models::observation::{Observation, ObservationFilter, ObservationSummary};
use crate::models::user::User;
use crate::routes::index::render_main;
use crate::timefmt::InTz;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::{Extension, Router, routing::get};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

const PAGE_SIZE: i64 = 10;
/// How many pages the « and » buttons skip.
const PAGE_JUMP: usize = 5;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(get_observations))
        .route(
            "/interferometry/{session_id}",
            axum::routing::delete(delete_interferometry_session),
        )
        .route(
            "/{observation_id}",
            get(get_observation_data).delete(delete_observation),
        )
        .route("/{observation_id}/csv", get(get_observation_csv))
        .route("/{observation_id}/fits", get(get_observation_fits))
        .with_state(state)
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<usize>,
    /// A user id, or "all" for every user's observations (admins only).
    /// Kept as a string so "all" doesn't fail deserialization.
    user_id: Option<String>,
    mode: Option<String>,
    /// Filter fields, as submitted by the filter form. Empty strings mean
    /// "no filter", since that is what an untouched form field sends.
    from: Option<String>,
    to: Option<String>,
    coord: Option<String>,
    telescope: Option<String>,
}

/// The archive filter as the form shows it, plus its parsed query form.
struct FilterForm {
    from: String,
    to: String,
    coord: String,
    telescope: String,
    filter: ObservationFilter,
}

impl FilterForm {
    /// Parse the filter from the query. Dates are calendar days in the
    /// viewer's timezone; `to` is inclusive. Unparseable values are dropped
    /// rather than rejected, so a mangled URL just shows a wider list.
    fn parse(query: &PageQuery, tz: Tz) -> Self {
        let date = |s: &Option<String>| {
            s.as_deref()
                .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
        };
        let text = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let midnight = |d: NaiveDate| {
            let naive = d.and_time(NaiveTime::MIN);
            tz.from_local_datetime(&naive)
                .earliest()
                .map(|t| t.timestamp())
                .unwrap_or_else(|| naive.and_utc().timestamp())
        };
        let from = date(&query.from);
        let to = date(&query.to);
        let coord = text(&query.coord);
        let telescope = text(&query.telescope);
        FilterForm {
            from: from.map(|d| d.to_string()).unwrap_or_default(),
            to: to.map(|d| d.to_string()).unwrap_or_default(),
            coord: coord.clone().unwrap_or_default(),
            telescope: telescope.clone().unwrap_or_default(),
            filter: ObservationFilter {
                start_after: from.map(midnight),
                start_before: to.and_then(|d| d.succ_opt()).map(midnight),
                coordinate_system: coord,
                telescope_id: telescope,
            },
        }
    }

    /// `&key=value` pairs for the active fields, appended to the page and
    /// delete links so they keep the filter.
    fn query_string(&self) -> String {
        [
            ("from", &self.from),
            ("to", &self.to),
            ("coord", &self.coord),
            ("telescope", &self.telescope),
        ]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("&{k}={}", url_encode(v)))
        .collect()
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct InterfSessionRow {
    id: i64,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    telescope_a: String,
    telescope_b: String,
    target_label: String,
    center_freq_mhz: f64,
}

#[derive(Template)]
#[template(path = "observations.html")]
struct ObservationsTemplate {
    lang: Language,
    mode: String,
    is_admin: bool,
    /// The `user_id` query value for links: a user id, or "all".
    viewed_user: String,
    /// `None` when an admin views all users' observations.
    viewed_user_id: Option<i64>,
    all_users: Vec<User>,
    show_interferometry_tab: bool,
    // single-dish fields
    observations: Vec<ObservationSummary>,
    current_page: usize,
    total_pages: usize,
    prev_page: Option<usize>,
    next_page: Option<usize>,
    /// Targets of the jump buttons, `None` when already on the first/last page.
    jump_back_page: Option<usize>,
    jump_forward_page: Option<usize>,
    /// Observations matching the filter.
    total_count: i64,
    /// Observations the user has in all, ignoring the filter.
    unfiltered_count: i64,
    filter: FilterForm,
    filter_active: bool,
    /// Built by [`FilterForm::query_string`].
    filter_qs: String,
    telescope_options: Vec<String>,
    coord_options: Vec<String>,
    // interferometry fields
    interferometry_sessions: Vec<InterfSessionRow>,
    /// Display timezone, used by `.in_tz(tz)` calls in the template.
    tz: Tz,
}

impl ObservationsTemplate {
    /// Whether to offer the filter at all. A student with a page or less of
    /// observations has nothing to filter, so they don't see the control.
    fn show_filter(&self) -> bool {
        self.is_admin || self.filter_active || self.unfiltered_count > PAGE_SIZE
    }

    fn coord_label(&self, coord: &str) -> String {
        match coord {
            "galactic" | "equatorial" | "horizontal" | "sun" => {
                self.lang.t(&format!("observe-coord-{coord}"))
            }
            other => other.to_string(),
        }
    }
}

fn make_interf_rows(
    sessions: Vec<InterferometrySession>,
    state: &AppState,
) -> Vec<InterfSessionRow> {
    sessions
        .into_iter()
        .map(|s| {
            let target_label = s.target_label_from_cache(&state.tle_cache);
            let center_freq_mhz = s.center_freq_hz / 1e6;
            InterfSessionRow {
                id: s.id,
                start_time: s.start_time,
                end_time: s.end_time,
                telescope_a: s.telescope_a,
                telescope_b: s.telescope_b,
                target_label,
                center_freq_mhz,
            }
        })
        .collect()
}

/// Which user's archive the request is about: admins may pick anyone, or
/// everyone (`None`). Anything unparseable falls back to the user's own.
fn viewed_user_id(user: &User, query: &PageQuery) -> Option<i64> {
    if !user.is_admin {
        return Some(user.id);
    }
    match query.user_id.as_deref().map(str::trim) {
        Some("all") => None,
        Some(id) => Some(id.parse().unwrap_or(user.id)),
        None => Some(user.id),
    }
}

/// Render the archive page body. Shared by the list view and the delete
/// handlers, which re-render in place after deleting. `mode` is the tab
/// asked for; it falls back to single-dish when there are no interferometry
/// sessions to show.
async fn render_observations(
    state: &AppState,
    lang: Language,
    user: &User,
    query: &PageQuery,
    mode: Option<&str>,
) -> Result<String, StatusCode> {
    let db = || state.database_connection.clone();
    let viewed_user_id = viewed_user_id(user, query);
    let tz = user.tz();
    let all_users = if user.is_admin {
        User::fetch_all_non_guest(db())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        vec![]
    };
    // The all-users view is single-dish only.
    let interf_count = match viewed_user_id {
        Some(id) => InterferometrySession::count_for_user(db(), id)
            .await
            .unwrap_or(0),
        None => 0,
    };
    let show_interferometry_tab = interf_count > 0;
    let mode = if mode == Some("interferometry") && show_interferometry_tab {
        "interferometry"
    } else {
        "single"
    };
    let filter = FilterForm::parse(query, tz);
    let filter_active = filter.filter.is_active();
    let filter_qs = filter.query_string();

    let mut observations = vec![];
    let mut total_count = 0;
    let mut unfiltered_count = 0;
    let mut current_page = 1;
    let mut telescope_options = vec![];
    let mut coord_options = vec![];
    let mut interferometry_sessions = vec![];
    if mode == "interferometry" {
        let sessions =
            InterferometrySession::fetch_for_user(db(), viewed_user_id.unwrap_or(user.id))
                .await
                .unwrap_or_default();
        interferometry_sessions = make_interf_rows(sessions, state);
    } else {
        total_count = Observation::count(db(), viewed_user_id, &filter.filter).await?;
        unfiltered_count = if filter_active {
            Observation::count(db(), viewed_user_id, &ObservationFilter::default()).await?
        } else {
            total_count
        };
        let total_pages = ((total_count as usize).saturating_sub(1) / PAGE_SIZE as usize) + 1;
        current_page = query.page.unwrap_or(1).clamp(1, total_pages);
        let offset = ((current_page - 1) as i64) * PAGE_SIZE;
        observations = Observation::fetch_summaries_page(
            db(),
            viewed_user_id,
            &filter.filter,
            PAGE_SIZE,
            offset,
        )
        .await?;
        (telescope_options, coord_options) =
            Observation::filter_options(db(), viewed_user_id).await?;
    }

    let total_pages = ((total_count as usize).saturating_sub(1) / PAGE_SIZE as usize) + 1;
    let prev_page = (current_page > 1).then(|| current_page - 1);
    let next_page = (current_page < total_pages).then(|| current_page + 1);
    let jump_back_page = prev_page.map(|_| current_page.saturating_sub(PAGE_JUMP).max(1));
    let jump_forward_page = next_page.map(|_| (current_page + PAGE_JUMP).min(total_pages));
    Ok(ObservationsTemplate {
        lang,
        mode: mode.to_string(),
        is_admin: user.is_admin,
        viewed_user: viewed_user_id.map_or("all".to_string(), |id| id.to_string()),
        viewed_user_id,
        all_users,
        show_interferometry_tab,
        observations,
        current_page,
        total_pages,
        prev_page,
        next_page,
        jump_back_page,
        jump_forward_page,
        total_count,
        unfiltered_count,
        filter,
        filter_active,
        filter_qs,
        telescope_options,
        coord_options,
        interferometry_sessions,
        tz,
    }
    .render()
    .expect("Template rendering should always succeed"))
}

async fn get_observations(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let Some(user) = user else {
        return Ok(if headers.get("hx-request").is_some() {
            ([("HX-Redirect", "/auth/login")], "").into_response()
        } else {
            Redirect::to("/auth/login").into_response()
        });
    };
    let content = render_observations(&state, lang, &user, &query, query.mode.as_deref()).await?;
    let content = if headers.get("hx-request").is_some() {
        content
    } else {
        render_main(Some(user), lang, content)
    };
    Ok(Html(content).into_response())
}

async fn delete_observation(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    Query(query): Query<PageQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    Observation::delete(state.database_connection.clone(), observation_id, &user).await?;
    let content = render_observations(&state, lang, &user, &query, Some("single")).await?;
    Ok(Html(content).into_response())
}

async fn delete_interferometry_session(
    Extension(lang): Extension<Language>,
    Extension(user): Extension<Option<User>>,
    Path(session_id): Path<i64>,
    Query(query): Query<PageQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let is_running = state
        .active_correlator
        .lock()
        .await
        .as_ref()
        .is_some_and(|c| c.session_id == session_id);
    if is_running {
        return Err(StatusCode::CONFLICT);
    }
    let deleted =
        InterferometrySession::delete(state.database_connection.clone(), session_id, &user)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }
    let content = render_observations(&state, lang, &user, &query, Some("interferometry")).await?;
    Ok(Html(content).into_response())
}

#[derive(Serialize)]
struct ObservationData {
    frequencies: Vec<f64>,
    amplitudes: Vec<f64>,
    telescope_id: String,
    coordinate_system: String,
    target_x: f64,
    target_y: f64,
    integration_time_secs: f64,
    start_time: String,
    vlsr_correction_mps: Option<f64>,
    az_offset_deg: Option<f64>,
    el_offset_deg: Option<f64>,
    azimuth_deg: Option<f64>,
    elevation_deg: Option<f64>,
}

async fn get_observation_data(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let user_id_filter = if user.is_admin { None } else { Some(user.id) };
    let observation =
        Observation::fetch_one(state.database_connection, observation_id, user_id_filter)
            .await?
            .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let horizontal = observation.horizontal();
    Ok(Json(ObservationData {
        frequencies,
        amplitudes,
        telescope_id: observation.telescope_id,
        coordinate_system: observation.coordinate_system,
        target_x: observation.target_x,
        target_y: observation.target_y,
        integration_time_secs: observation.integration_time_secs,
        start_time: observation.start_time.to_rfc3339(),
        vlsr_correction_mps: observation.vlsr_correction_mps,
        az_offset_deg: observation.az_offset_deg,
        el_offset_deg: observation.el_offset_deg,
        azimuth_deg: horizontal.map(|(az, _)| az),
        elevation_deg: horizontal.map(|(_, el)| el),
    })
    .into_response())
}

async fn get_observation_csv(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let user_id_filter = if user.is_admin { None } else { Some(user.id) };
    let observation =
        Observation::fetch_one(state.database_connection, observation_id, user_id_filter)
            .await?
            .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let has_vlsr = observation.vlsr_correction_mps.is_some();
    let vlsr_mps = observation.vlsr_correction_mps.unwrap_or(0.0);
    let c = 299_792_458.0_f64;
    let f_rest = 1_420_405_751.77_f64;

    let tag = observation.start_time.format("%Y%m%dT%H%M%S").to_string();
    let filename = format!("SALSA-{}-{}.csv", observation.telescope_id, tag);

    let mut csv = String::new();
    csv.push_str("# Origin: SALSA\n");
    csv.push_str(&format!("# Telescope: {}\n", observation.telescope_id));
    csv.push_str(&format!(
        "# Date: {}\n",
        observation.start_time.to_rfc3339()
    ));
    csv.push_str(&format!(
        "# Coordinate system: {}\n",
        observation.coordinate_system
    ));
    csv.push_str(&format!(
        "# Target: {:.4}, {:.4} deg\n",
        observation.target_x, observation.target_y
    ));
    if let Some((az, el)) = observation.horizontal() {
        csv.push_str(&format!("# Azimuth at start: {az:.2} deg\n"));
        csv.push_str(&format!("# Elevation at start: {el:.2} deg\n"));
    }
    csv.push_str(&format!(
        "# Integration time: {:.0} s\n",
        observation.integration_time_secs
    ));
    // Receiver settings, so a spectrum can be told apart from one taken at a
    // different gain or in a different mode. Absent for observations recorded
    // before these were stored, hence emitting each line only when present
    // rather than writing a placeholder that could be mistaken for a value.
    if let Some(mode) = &observation.observation_mode {
        csv.push_str(&format!("# Observation mode: {mode}\n"));
    }
    if let Some(v) = observation.center_freq_hz {
        csv.push_str(&format!("# Center frequency: {:.6} MHz\n", v / 1e6));
    }
    if let Some(v) = observation.ref_freq_hz {
        csv.push_str(&format!("# Reference frequency: {:.6} MHz\n", v / 1e6));
    }
    if let Some(v) = observation.bandwidth_hz {
        csv.push_str(&format!("# Bandwidth: {:.4} MHz\n", v / 1e6));
    }
    if let Some(v) = observation.spectral_channels {
        csv.push_str(&format!("# Spectral channels: {v}\n"));
    }
    if let Some(v) = observation.gain_db {
        csv.push_str(&format!("# Receiver gain: {v:.1} dB\n"));
    }
    if let Some(v) = observation.rfi_filter {
        csv.push_str(&format!("# RFI filter: {}\n", if v { "on" } else { "off" }));
    }
    if has_vlsr {
        csv.push_str(&format!("# VLSR correction: {:.2} m/s\n", vlsr_mps));
        csv.push_str("# Columns: frequency_hz,amplitude,vlsr_mps\n");
        csv.push_str("frequency_hz,amplitude,vlsr_mps\n");
        for (freq, amp) in frequencies.iter().zip(amplitudes.iter()) {
            let vlsr = -(freq - f_rest) * c / f_rest + vlsr_mps;
            csv.push_str(&format!("{},{},{:.4}\n", freq, amp, vlsr));
        }
    } else {
        csv.push_str("# VLSR correction: not available\n");
        csv.push_str("# Columns: frequency_hz,amplitude\n");
        csv.push_str("frequency_hz,amplitude\n");
        for (freq, amp) in frequencies.iter().zip(amplitudes.iter()) {
            csv.push_str(&format!("{},{}\n", freq, amp));
        }
    }

    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        csv,
    )
        .into_response())
}

async fn get_observation_fits(
    Extension(user): Extension<Option<User>>,
    Path(observation_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let user = user.ok_or(StatusCode::UNAUTHORIZED)?;
    let user_id_filter = if user.is_admin { None } else { Some(user.id) };
    let observation =
        Observation::fetch_one(state.database_connection, observation_id, user_id_filter)
            .await?
            .ok_or(StatusCode::NOT_FOUND)?;

    let frequencies: Vec<f64> = serde_json::from_str(&observation.frequencies_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amplitudes: Vec<f64> = serde_json::from_str(&observation.amplitudes_json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tag = observation.start_time.format("%Y%m%dT%H%M%S").to_string();
    let filename = format!("SALSA-{}-{}.fits", observation.telescope_id, tag);

    let horizontal = observation.horizontal();
    let fits_bytes = write_spectrum_fits(&SpectrumMeta {
        frequencies: &frequencies,
        amplitudes: &amplitudes,
        telescope_id: &observation.telescope_id,
        coordinate_system: &observation.coordinate_system,
        target_x: observation.target_x,
        target_y: observation.target_y,
        integration_time_secs: observation.integration_time_secs,
        start_time: &observation
            .start_time
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
        vlsr_correction_mps: observation.vlsr_correction_mps,
        azimuth_deg: horizontal.map(|(az, _)| az),
        elevation_deg: horizontal.map(|(_, el)| el),
    });

    Ok((
        [
            (header::CONTENT_TYPE, "application/fits"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        fits_bytes,
    )
        .into_response())
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn query(from: &str, to: &str, coord: &str, telescope: &str) -> PageQuery {
        PageQuery {
            page: None,
            user_id: None,
            mode: None,
            from: Some(from.to_string()),
            to: Some(to.to_string()),
            coord: Some(coord.to_string()),
            telescope: Some(telescope.to_string()),
        }
    }

    #[test]
    fn empty_form_is_no_filter() {
        let form = FilterForm::parse(&query("", "", "", " "), chrono_tz::UTC);
        assert!(!form.filter.is_active());
        assert_eq!(form.query_string(), "");
    }

    #[test]
    fn dates_are_local_days_with_inclusive_end() {
        let tz: Tz = "Europe/Stockholm".parse().unwrap();
        let form = FilterForm::parse(&query("2026-09-20", "2026-09-21", "", ""), tz);
        // Stockholm is UTC+2 in September: local midnight is 22:00 UTC the day before.
        let expect = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().timestamp();
        assert_eq!(
            form.filter.start_after,
            Some(expect("2026-09-19T22:00:00Z"))
        );
        assert_eq!(
            form.filter.start_before,
            Some(expect("2026-09-21T22:00:00Z"))
        );
    }

    #[test]
    fn garbage_dates_are_ignored_and_values_are_encoded() {
        let form = FilterForm::parse(&query("yesterday", "", "sun", "salsa a&b"), chrono_tz::UTC);
        assert_eq!(form.filter.start_after, None);
        assert_eq!(form.query_string(), "&coord=sun&telescope=salsa%20a%26b");
    }
}
