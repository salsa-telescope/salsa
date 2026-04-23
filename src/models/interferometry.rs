use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::user::User;

// ---------------------------------------------------------------------------
// Interferometry session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InterferometrySession {
    pub id: i64,
    pub user_id: i64,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub telescope_a: String,
    pub telescope_b: String,
    pub coordinate_system: String,
    pub target_x: f64,
    pub target_y: f64,
    pub center_freq_hz: f64,
    pub bandwidth_hz: f64,
}

impl InterferometrySession {
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: &User,
        telescope_a: String,
        telescope_b: String,
        coordinate_system: String,
        target_x: f64,
        target_y: f64,
        center_freq_hz: f64,
        bandwidth_hz: f64,
    ) -> Result<i64, InternalError> {
        let conn = connection.lock().await;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO interferometry_session
             (user_id, start_time, telescope_a, telescope_b,
              coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                user.id,
                now,
                telescope_a,
                telescope_b,
                coordinate_system,
                target_x,
                target_y,
                center_freq_hz,
                bandwidth_hz
            ],
        )
        .map_err(|e| InternalError::new(format!("Failed to create session: {e}")))?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn finalize(
        connection: Arc<Mutex<Connection>>,
        id: i64,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE interferometry_session SET end_time = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )
        .map_err(|e| InternalError::new(format!("Failed to finalize session: {e}")))?;
        Ok(())
    }

    pub async fn fetch_for_user(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<Vec<Self>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, start_time, end_time, telescope_a, telescope_b,
                        coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz
                 FROM interferometry_session
                 WHERE user_id = ?1
                 ORDER BY start_time DESC",
            )
            .map_err(|e| InternalError::new(format!("prepare: {e}")))?;
        stmt.query_map([user_id], map_session_row)
            .map_err(|e| InternalError::new(format!("query: {e}")))?
            .collect::<Result<_, _>>()
            .map_err(|e| InternalError::new(format!("row: {e}")))
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user_id_filter: Option<i64>,
    ) -> Result<Option<Self>, InternalError> {
        const SELECT_BY_ID: &str = "SELECT id, user_id, start_time, end_time, telescope_a, telescope_b,
                    coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz
             FROM interferometry_session WHERE id = ?1";
        const SELECT_BY_ID_AND_USER: &str = "SELECT id, user_id, start_time, end_time, telescope_a, telescope_b,
                    coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz
             FROM interferometry_session WHERE id = ?1 AND user_id = ?2";
        let conn = connection.lock().await;
        let result = match user_id_filter {
            Some(uid) => conn.prepare(SELECT_BY_ID_AND_USER).and_then(|mut stmt| {
                stmt.query_row(rusqlite::params![id, uid], map_session_row)
                    .optional()
            }),
            None => conn.prepare(SELECT_BY_ID).and_then(|mut stmt| {
                stmt.query_row(rusqlite::params![id], map_session_row)
                    .optional()
            }),
        };
        result.map_err(|e| InternalError::new(format!("fetch session: {e}")))
    }

    pub async fn delete(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user: &User,
    ) -> Result<bool, InternalError> {
        // Child `interferometry_visibility` rows are removed by ON DELETE CASCADE
        // (enforced globally by `PRAGMA foreign_keys = ON` in database.rs).
        let conn = connection.lock().await;
        let rows = if user.is_admin {
            conn.execute(
                "DELETE FROM interferometry_session WHERE id = ?1",
                rusqlite::params![id],
            )
        } else {
            conn.execute(
                "DELETE FROM interferometry_session WHERE id = ?1 AND user_id = ?2",
                rusqlite::params![id, user.id],
            )
        }
        .map_err(|e| InternalError::new(format!("delete session: {e}")))?;
        Ok(rows > 0)
    }

    pub async fn count_for_user(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<i64, InternalError> {
        let conn = connection.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM interferometry_session WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .map_err(|e| InternalError::new(format!("count sessions: {e}")))
    }

    pub fn target_label(&self, satellite_name: Option<String>) -> String {
        match self.coordinate_system.as_str() {
            "gnss" => format!(
                "gnss ({})",
                satellite_name.unwrap_or_else(|| format!("NORAD {}", self.target_x as u64))
            ),
            "sun" => "sun".to_string(),
            "stow" => "stow".to_string(),
            cs => format!("{} ({:.1}, {:.1})", cs, self.target_x, self.target_y),
        }
    }

    /// Look up the satellite name (if this session targets a GNSS satellite) and
    /// render a human-readable target label.
    pub fn target_label_from_cache(
        &self,
        tle_cache: &crate::tle_cache::TleCacheHandle,
    ) -> String {
        let sat_name = if self.coordinate_system == "gnss" {
            tle_cache.satellite_name(self.target_x as u64)
        } else {
            None
        };
        self.target_label(sat_name)
    }
}

fn timestamp_from_secs(idx: usize, secs: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(secs, 0).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Integer,
            format!("invalid unix timestamp {secs}").into(),
        )
    })
}

fn timestamp_from_millis(idx: usize, ms: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(ms).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Integer,
            format!("invalid unix timestamp (ms) {ms}").into(),
        )
    })
}

fn map_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InterferometrySession> {
    let end_ts: Option<i64> = row.get(3)?;
    let end_time = end_ts.map(|t| timestamp_from_secs(3, t)).transpose()?;
    Ok(InterferometrySession {
        id: row.get(0)?,
        user_id: row.get(1)?,
        start_time: timestamp_from_secs(2, row.get(2)?)?,
        end_time,
        telescope_a: row.get(4)?,
        telescope_b: row.get(5)?,
        coordinate_system: row.get(6)?,
        target_x: row.get(7)?,
        target_y: row.get(8)?,
        center_freq_hz: row.get(9)?,
        bandwidth_hz: row.get(10)?,
    })
}

// ---------------------------------------------------------------------------
// Interferometry visibility (one row per 1-second integration)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InterferometryVisibility {
    pub id: i64,
    pub session_id: i64,
    pub time: DateTime<Utc>,
    pub mean_amplitude: f64,
    pub mean_phase_deg: f64,
    pub delay_ns: f64,
    pub amplitudes_json: String,
    pub phases_json: String,
    pub frequencies_json: String,
}

impl InterferometryVisibility {
    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        connection: Arc<Mutex<Connection>>,
        session_id: i64,
        time: DateTime<Utc>,
        mean_amplitude: f64,
        mean_phase_deg: f64,
        delay_ns: f64,
        amplitudes_json: String,
        phases_json: String,
        frequencies_json: String,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO interferometry_visibility
             (session_id, time, mean_amplitude, mean_phase_deg, delay_ns,
              amplitudes_json, phases_json, frequencies_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session_id,
                time.timestamp_millis(),
                mean_amplitude,
                mean_phase_deg,
                delay_ns,
                amplitudes_json,
                phases_json,
                frequencies_json,
            ],
        )
        .map_err(|e| InternalError::new(format!("insert visibility: {e}")))?;
        Ok(())
    }

    pub async fn fetch_for_session(
        connection: Arc<Mutex<Connection>>,
        session_id: i64,
        after_id: i64,
    ) -> Result<Vec<Self>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, time, mean_amplitude, mean_phase_deg, delay_ns,
                        amplitudes_json, phases_json, frequencies_json
                 FROM interferometry_visibility
                 WHERE session_id = ?1 AND id > ?2
                 ORDER BY id ASC",
            )
            .map_err(|e| InternalError::new(format!("prepare: {e}")))?;
        stmt.query_map(rusqlite::params![session_id, after_id], |row| {
            Ok(InterferometryVisibility {
                id: row.get(0)?,
                session_id: row.get(1)?,
                time: timestamp_from_millis(2, row.get(2)?)?,
                mean_amplitude: row.get(3)?,
                mean_phase_deg: row.get(4)?,
                delay_ns: row.get(5)?,
                amplitudes_json: row.get(6)?,
                phases_json: row.get(7)?,
                frequencies_json: row.get(8)?,
            })
        })
        .map_err(|e| InternalError::new(format!("query: {e}")))?
        .collect::<Result<_, _>>()
        .map_err(|e| InternalError::new(format!("row: {e}")))
    }

    pub async fn count_for_session(
        connection: Arc<Mutex<Connection>>,
        session_id: i64,
    ) -> Result<i64, InternalError> {
        let conn = connection.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM interferometry_visibility WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| InternalError::new(format!("count visibilities: {e}")))
    }
}
