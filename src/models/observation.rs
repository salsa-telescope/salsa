use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::coords::{ONSALA_LOCATION, horizontal_from_equatorial, horizontal_from_galactic};
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
    pub vlsr_correction_mps: Option<f64>,
    pub az_offset_deg: Option<f64>,
    pub el_offset_deg: Option<f64>,
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
        vlsr_correction_mps: Option<f64>,
        az_offset_deg: Option<f64>,
        el_offset_deg: Option<f64>,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO observation (user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json, vlsr_correction_mps, az_offset_deg, el_offset_deg)
                 VALUES ((?1), (?2), (?3), (?4), (?5), (?6), (?7), (?8), (?9), (?10), (?11), (?12))",
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
                vlsr_correction_mps,
                az_offset_deg,
                el_offset_deg,
            ),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert observation in db: {err}")))?;
        Ok(())
    }

    pub async fn fetch_for_user_page(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        page_size: i64,
        offset: i64,
    ) -> Result<Vec<Observation>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json, vlsr_correction_mps, az_offset_deg, el_offset_deg
                 FROM observation
                 WHERE user_id = (?1)
                 ORDER BY start_time DESC
                 LIMIT (?2) OFFSET (?3)",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let observations = stmt
            .query_map(rusqlite::params![user_id, page_size, offset], |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    telescope_id: row.get(2)?,
                    start_time: DateTime::<Utc>::from_timestamp(row.get(3)?, 0).unwrap_or_default(),
                    coordinate_system: row.get(4)?,
                    target_x: row.get(5)?,
                    target_y: row.get(6)?,
                    integration_time_secs: row.get(7)?,
                    frequencies_json: row.get(8)?,
                    amplitudes_json: row.get(9)?,
                    vlsr_correction_mps: row.get(10)?,
                    az_offset_deg: row.get(11)?,
                    el_offset_deg: row.get(12)?,
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

    pub async fn count_for_user(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<i64, InternalError> {
        let conn = connection.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM observation WHERE user_id = (?1)",
            [user_id],
            |row| row.get(0),
        )
        .map_err(|err| InternalError::new(format!("Failed to count observations: {err}")))
    }

    pub async fn delete(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user: &User,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "DELETE FROM observation WHERE id = (?1) AND user_id = (?2)",
            [&id, &user.id],
        )
        .map_err(|err| InternalError::new(format!("Failed to delete observation: {err}")))?;
        Ok(())
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        user_id: Option<i64>,
    ) -> Result<Option<Observation>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json, vlsr_correction_mps, az_offset_deg, el_offset_deg
                 FROM observation
                 WHERE id = (?1) AND ((?2) IS NULL OR user_id = (?2))",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let mut observations = stmt
            .query_map(rusqlite::params![id, user_id], |row| {
                Ok(Observation {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    telescope_id: row.get(2)?,
                    start_time: DateTime::<Utc>::from_timestamp(row.get(3)?, 0).unwrap_or_default(),
                    coordinate_system: row.get(4)?,
                    target_x: row.get(5)?,
                    target_y: row.get(6)?,
                    integration_time_secs: row.get(7)?,
                    frequencies_json: row.get(8)?,
                    amplitudes_json: row.get(9)?,
                    vlsr_correction_mps: row.get(10)?,
                    az_offset_deg: row.get(11)?,
                    el_offset_deg: row.get(12)?,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?;

        match observations.next() {
            Some(Ok(obs)) => Ok(Some(obs)),
            Some(Err(err)) => Err(InternalError::new(format!("Failed to map row: {err}"))),
            None => Ok(None),
        }
    }

    /// Commanded azimuth/elevation in degrees at the start of the
    /// observation, including any pointing offsets. Horizontal-type
    /// targets (horizontal, sun, gnss) store az/el as the target
    /// coordinates; equatorial and galactic targets are converted for
    /// the SALSA site at `start_time`, reconstructing the same pointing
    /// math the telescope used. This is the commanded direction, not a
    /// readback — a mechanically stuck telescope would still report it.
    pub fn horizontal(&self) -> Option<(f64, f64)> {
        let (az_deg, el_deg) = match self.coordinate_system.as_str() {
            "horizontal" | "sun" => (self.target_x, self.target_y),
            s if s.starts_with("gnss") => (self.target_x, self.target_y),
            "equatorial" => {
                let dir = horizontal_from_equatorial(
                    ONSALA_LOCATION,
                    self.start_time,
                    self.target_x.to_radians(),
                    self.target_y.to_radians(),
                );
                (dir.azimuth.to_degrees(), dir.elevation.to_degrees())
            }
            "galactic" => {
                let dir = horizontal_from_galactic(
                    ONSALA_LOCATION,
                    self.start_time,
                    self.target_x.to_radians(),
                    self.target_y.to_radians(),
                );
                (dir.azimuth.to_degrees(), dir.elevation.to_degrees())
            }
            _ => return None,
        };
        Some((
            (az_deg + self.az_offset_deg.unwrap_or(0.0)).rem_euclid(360.0),
            el_deg + self.el_offset_deg.unwrap_or(0.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn observation(coordinate_system: &str, target_x: f64, target_y: f64) -> Observation {
        Observation {
            id: 1,
            user_id: 1,
            telescope_id: "test".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 7, 12, 12, 0, 0).unwrap(),
            coordinate_system: coordinate_system.to_string(),
            target_x,
            target_y,
            integration_time_secs: 60.0,
            frequencies_json: "[]".to_string(),
            amplitudes_json: "[]".to_string(),
            vlsr_correction_mps: None,
            az_offset_deg: None,
            el_offset_deg: None,
        }
    }

    #[test]
    fn galactic_horizontal_matches_pointing_math() {
        let obs = observation("galactic", 140.0, 0.0);
        let dir =
            horizontal_from_galactic(ONSALA_LOCATION, obs.start_time, 140.0_f64.to_radians(), 0.0);
        let (az, el) = obs.horizontal().unwrap();
        assert!((az - dir.azimuth.to_degrees()).abs() < 1e-9);
        assert!((el - dir.elevation.to_degrees()).abs() < 1e-9);
    }

    #[test]
    fn horizontal_targets_pass_through_and_apply_offsets() {
        let mut obs = observation("sun", 180.0, 45.0);
        obs.az_offset_deg = Some(1.5);
        obs.el_offset_deg = Some(-0.5);
        assert_eq!(obs.horizontal(), Some((181.5, 44.5)));
    }

    #[test]
    fn azimuth_wraps_around_north() {
        let mut obs = observation("horizontal", 359.0, 30.0);
        obs.az_offset_deg = Some(2.0);
        let (az, _) = obs.horizontal().unwrap();
        assert!((az - 1.0).abs() < 1e-9);
    }

    #[test]
    fn gnss_and_unknown_systems() {
        assert_eq!(
            observation("gnss:GPS BIII-6", 120.0, 60.0).horizontal(),
            Some((120.0, 60.0))
        );
        assert_eq!(observation("stow", 0.0, 0.0).horizontal(), None);
    }
}
