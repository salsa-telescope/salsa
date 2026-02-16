use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::user::User;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Observation {
    pub id: i64,
    pub user_id: i64,
    pub telescope_id: String,
    pub start_time: DateTime<Utc>,
    pub coordinate_system: String,
    pub target_x: f64,
    pub target_y: f64,
    pub integration_time_secs: f64,
    pub frequencies_json: String,
    pub amplitudes_json: String,
}

impl Observation {
#[allow(clippy::too_many_arguments)]
    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: &User,
        telescope_id: &str,
        start_time: DateTime<Utc>,
        coordinate_system: &str,
        target_x: f64,
        target_y: f64,
        integration_time_secs: f64,
        frequencies_json: &str,
        amplitudes_json: &str,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO observation (user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json)
                 VALUES ((?1), (?2), (?3), (?4), (?5), (?6), (?7), (?8), (?9))",
            (
                &user.id,
                telescope_id,
                start_time.timestamp(),
                coordinate_system,
                target_x,
                target_y,
                integration_time_secs,
                frequencies_json,
                amplitudes_json,
            ),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert observation in db: {err}")))?;
        Ok(())
    }

    pub async fn fetch_for_user(
        connection: Arc<Mutex<Connection>>,
        user: &User,
    ) -> Result<Vec<Observation>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json
                 FROM observation
                 WHERE user_id = (?1)
                 ORDER BY start_time DESC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let observations = stmt
            .query_map([&user.id], |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    telescope_id: row.get(2)?,
                    start_time: DateTime::<Utc>::from_timestamp(row.get(3)?, 0).unwrap(),
                    coordinate_system: row.get(4)?,
                    target_x: row.get(5)?,
                    target_y: row.get(6)?,
                    integration_time_secs: row.get(7)?,
                    frequencies_json: row.get(8)?,
                    amplitudes_json: row.get(9)?,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?;

        let mut res = Vec::new();
        for obs in observations {
            match obs {
                Ok(obs) => res.push(obs),
                Err(err) => {
                    return Err(InternalError::new(format!("Failed to map row: {err}")));
                }
            }
        }
        Ok(res)
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user: &User,
    ) -> Result<Option<Observation>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json
                 FROM observation
                 WHERE id = (?1) AND user_id = (?2)",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let mut observations = stmt
            .query_map([&id, &user.id], |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    telescope_id: row.get(2)?,
                    start_time: DateTime::<Utc>::from_timestamp(row.get(3)?, 0).unwrap(),
                    coordinate_system: row.get(4)?,
                    target_x: row.get(5)?,
                    target_y: row.get(6)?,
                    integration_time_secs: row.get(7)?,
                    frequencies_json: row.get(8)?,
                    amplitudes_json: row.get(9)?,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?;

        match observations.next() {
            Some(Ok(obs)) => Ok(Some(obs)),
            Some(Err(err)) => Err(InternalError::new(format!("Failed to map row: {err}"))),
            None => Ok(None),
        }
    }
}
