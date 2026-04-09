use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::user::User;

// ---------------------------------------------------------------------------
// Interferometry session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let conn = connection.lock().await;
        let sql = if user_id_filter.is_some() {
            "SELECT id, user_id, start_time, end_time, telescope_a, telescope_b,
                    coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz
             FROM interferometry_session WHERE id = ?1 AND user_id = ?2"
        } else {
            "SELECT id, user_id, start_time, end_time, telescope_a, telescope_b,
                    coordinate_system, target_x, target_y, center_freq_hz, bandwidth_hz
             FROM interferometry_session WHERE id = ?1 AND 1 = ?2"
        };
        let param2: i64 = user_id_filter.unwrap_or(1);
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| InternalError::new(format!("prepare: {e}")))?;
        Ok(stmt
            .query_map([id, param2], map_session_row)
            .map_err(|e| InternalError::new(format!("query: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| InternalError::new(format!("row: {e}")))?
            .pop())
    }

    pub async fn delete(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user: &User,
    ) -> Result<bool, InternalError> {
        let conn = connection.lock().await;
        let rows = conn
            .execute(
                "DELETE FROM interferometry_session WHERE id = ?1 AND user_id = ?2",
                rusqlite::params![id, user.id],
            )
            .map_err(|e| InternalError::new(format!("delete session: {e}")))?;
        Ok(rows > 0)
    }
}

fn map_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InterferometrySession> {
    let end_ts: Option<i64> = row.get(3)?;
    Ok(InterferometrySession {
        id: row.get(0)?,
        user_id: row.get(1)?,
        start_time: DateTime::<Utc>::from_timestamp(row.get(2)?, 0).unwrap_or_default(),
        end_time: end_ts.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    ) -> Result<Vec<Self>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, time, mean_amplitude, mean_phase_deg, delay_ns,
                        amplitudes_json, phases_json, frequencies_json
                 FROM interferometry_visibility
                 WHERE session_id = ?1
                 ORDER BY time ASC",
            )
            .map_err(|e| InternalError::new(format!("prepare: {e}")))?;
        stmt.query_map([session_id], |row| {
            Ok(InterferometryVisibility {
                id: row.get(0)?,
                session_id: row.get(1)?,
                time: DateTime::<Utc>::from_timestamp_millis(row.get(2)?).unwrap_or_default(),
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
}
