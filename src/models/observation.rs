use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::coords::{ONSALA_LOCATION, horizontal_from_equatorial, horizontal_from_galactic};
use crate::error::InternalError;
use crate::models::telescope_types::ReceiverConfiguration;
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
    /// Receiver settings the spectrum was taken with. `None` for observations
    /// recorded before these were stored (migration V18).
    pub gain_db: Option<f64>,
    pub center_freq_hz: Option<f64>,
    pub ref_freq_hz: Option<f64>,
    pub bandwidth_hz: Option<f64>,
    pub spectral_channels: Option<i64>,
    pub observation_mode: Option<String>,
    pub rfi_filter: Option<bool>,
}

/// The metadata the archive list page renders, without the spectrum blobs.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ObservationSummary {
    pub id: i64,
    pub telescope_id: String,
    pub start_time: DateTime<Utc>,
    pub coordinate_system: String,
    pub target_x: f64,
    pub target_y: f64,
    pub integration_time_secs: f64,
}

/// Every column of `observation`, in the order [`map_observation_row`] reads
/// them. Kept next to the mapper so the two cannot drift apart.
const OBSERVATION_COLUMNS: &str = "id, user_id, telescope_id, start_time, coordinate_system, \
     target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json, \
     vlsr_correction_mps, az_offset_deg, el_offset_deg, gain_db, center_freq_hz, ref_freq_hz, \
     bandwidth_hz, spectral_channels, observation_mode, rfi_filter";

fn map_observation_row(row: &rusqlite::Row) -> rusqlite::Result<Observation> {
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
        gain_db: row.get(13)?,
        center_freq_hz: row.get(14)?,
        ref_freq_hz: row.get(15)?,
        bandwidth_hz: row.get(16)?,
        spectral_channels: row.get(17)?,
        observation_mode: row.get(18)?,
        rfi_filter: row.get(19)?,
    })
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
        receiver: &ReceiverConfiguration,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO observation (user_id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs, frequencies_json, amplitudes_json, vlsr_correction_mps, az_offset_deg, el_offset_deg, gain_db, center_freq_hz, ref_freq_hz, bandwidth_hz, spectral_channels, observation_mode, rfi_filter)
                 VALUES ((?1), (?2), (?3), (?4), (?5), (?6), (?7), (?8), (?9), (?10), (?11), (?12), (?13), (?14), (?15), (?16), (?17), (?18), (?19))",
            // `params!` rather than a tuple: rusqlite only implements `Params`
            // for tuples up to 16 elements, and this is past that.
            rusqlite::params![
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
                receiver.gain_db,
                receiver.center_freq_hz,
                receiver.ref_freq_hz,
                receiver.bandwidth_hz,
                receiver.spectral_channels as i64,
                format!("{:?}", receiver.mode),
                receiver.rfi_filter,
            ],
        )
        .map_err(|err| InternalError::new(format!("Failed to insert observation in db: {err}")))?;
        Ok(())
    }

    /// One page of the archive list.
    ///
    /// Deliberately returns [`ObservationSummary`] rather than `Observation`:
    /// the list template renders metadata only, and the two spectrum columns
    /// are several tens of kilobytes of JSON per row that would be read off
    /// disk and parsed just to be dropped. The full row is only ever needed by
    /// the download endpoints, which go through [`Observation::fetch_one`].
    pub async fn fetch_summaries_for_user_page(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        page_size: i64,
        offset: i64,
    ) -> Result<Vec<ObservationSummary>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, telescope_id, start_time, coordinate_system, target_x, target_y, integration_time_secs
                 FROM observation
                 WHERE user_id = (?1)
                 ORDER BY start_time DESC
                 LIMIT (?2) OFFSET (?3)",
            )?;
        let summaries = stmt.query_map(rusqlite::params![user_id, page_size, offset], |row| {
            Ok(ObservationSummary {
                id: row.get(0)?,
                telescope_id: row.get(1)?,
                start_time: DateTime::<Utc>::from_timestamp(row.get(2)?, 0).unwrap_or_default(),
                coordinate_system: row.get(3)?,
                target_x: row.get(4)?,
                target_y: row.get(5)?,
                integration_time_secs: row.get(6)?,
            })
        })?;

        summaries
            .collect::<Result<Vec<_>, _>>()
            .map_err(InternalError::from)
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
        let mut stmt = conn.prepare(&format!(
            "SELECT {OBSERVATION_COLUMNS}
                 FROM observation
                 WHERE id = (?1) AND ((?2) IS NULL OR user_id = (?2))"
        ))?;
        let mut observations =
            stmt.query_map(rusqlite::params![id, user_id], map_observation_row)?;

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
            gain_db: None,
            center_freq_hz: None,
            ref_freq_hz: None,
            bandwidth_hz: None,
            spectral_channels: None,
            observation_mode: None,
            rfi_filter: None,
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
