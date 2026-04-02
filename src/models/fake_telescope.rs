use crate::coords::{Direction, Location};
use crate::coords::{horizontal_from_equatorial, horizontal_from_galactic, horizontal_from_sun};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    ObservedSpectra, ReceiverConfiguration, ReceiverError, TelescopeError, TelescopeInfo,
    TelescopeStatus, TelescopeTarget,
};
use crate::tle_cache::TleCacheHandle;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::Rng;
use rand_distr::StandardNormal;
use std::f64::consts::PI;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace};

const FAKE_TELESCOPE_PARKING_HORIZONTAL: Direction = Direction {
    azimuth: 0.0,
    elevation: PI / 2.0,
};

pub const FAKE_TELESCOPE_SLEWING_SPEED: f64 = PI / 10.0;
pub const FAKE_TELESCOPE_CHANNELS: usize = 400;
pub const FAKE_TELESCOPE_CHANNEL_WIDTH: f64 = 2e6f64 / FAKE_TELESCOPE_CHANNELS as f64;
pub const FAKE_TELESCOPE_FIRST_CHANNEL: f64 =
    1.420e9f64 - FAKE_TELESCOPE_CHANNEL_WIDTH * FAKE_TELESCOPE_CHANNELS as f64 / 2f64;
pub const FAKE_TELESCOPE_NOISE: f64 = 2f64;
pub const TELESCOPE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

struct Inner {
    target: Option<TelescopeTarget>,
    az_offset_rad: f64,
    el_offset_rad: f64,
    horizontal: Direction,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
    webcam_crop: Option<[f64; 4]>,
    most_recent_error: Option<TelescopeError>,
    receiver_configuration: ReceiverConfiguration,
    current_spectra: Vec<ObservedSpectra>,
    name: String,
    stow_position: Option<Direction>,
    alive: bool,
    tle_cache: TleCacheHandle,
}

pub struct FakeTelescope {
    inner: Arc<Mutex<Inner>>,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    name: String,
    stow_position: Option<Direction>,
    location: Location,
    min_elevation_rad: f64,
    max_elevation_rad: f64,
    webcam_crop: Option<[f64; 4]>,
    default_ref_freq_hz: f64,
    default_gain_db: f64,
    tle_cache: TleCacheHandle,
) -> FakeTelescope {
    let inner = Arc::new(Mutex::new(Inner {
        target: None,
        az_offset_rad: 0.0,
        el_offset_rad: 0.0,
        horizontal: FAKE_TELESCOPE_PARKING_HORIZONTAL,
        location,
        min_elevation_rad,
        max_elevation_rad,
        webcam_crop,
        most_recent_error: None,
        receiver_configuration: ReceiverConfiguration {
            integrate: false,
            ref_freq_hz: default_ref_freq_hz,
            gain_db: default_gain_db,
            ..Default::default()
        },
        current_spectra: vec![],
        name,
        stow_position,
        alive: true,
        tle_cache,
    }));

    let task_inner = inner.clone();
    tokio::spawn(async move {
        loop {
            {
                let mut inner = task_inner.lock().await;
                if let Err(error) = inner.update(TELESCOPE_UPDATE_INTERVAL) {
                    error!("Failed to update telescope: {}", error);
                }
            }
            tokio::time::sleep(TELESCOPE_UPDATE_INTERVAL).await;
        }
    });

    FakeTelescope { inner }
}

#[async_trait]
impl Telescope for FakeTelescope {
    async fn set_target(
        &self,
        target: TelescopeTarget,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError> {
        let mut inner = self.inner.lock().await;

        inner.most_recent_error = None;
        inner.receiver_configuration.integrate = false;
        inner.current_spectra.clear();

        let raw = calculate_target_horizontal(inner.location, Utc::now(), target, &inner.tle_cache)
            .unwrap_or(Direction {
                azimuth: 0.0,
                elevation: -1.0,
            });
        let target_horizontal = apply_offset(raw, az_offset_rad, el_offset_rad);
        if target_horizontal.elevation < inner.min_elevation_rad
            || target_horizontal.elevation > inner.max_elevation_rad
        {
            info!(
                "Refusing to set target for telescope {} to {:?}. Target is out of elevation range",
                &inner.name, &target
            );
            Err(TelescopeError::TargetOutOfElevationRange {
                min_deg: inner.min_elevation_rad.to_degrees(),
                max_deg: inner.max_elevation_rad.to_degrees(),
            })
        } else {
            info!(
                "Setting target for telescope {} to {:?}",
                &inner.name, &target
            );
            inner.az_offset_rad = az_offset_rad;
            inner.el_offset_rad = el_offset_rad;
            inner.target = Some(target);
            Ok(target)
        }
    }

    async fn stop(&self) -> Result<(), TelescopeError> {
        let mut inner = self.inner.lock().await;
        info!("Stopping telescope {}", &inner.name);
        inner.target = None;
        Ok(())
    }

    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError> {
        let mut inner = self.inner.lock().await;

        if receiver_configuration.integrate && !inner.receiver_configuration.integrate {
            info!("Starting integration");
            inner.current_spectra.clear();
            inner.receiver_configuration.integrate = true;
        } else if !receiver_configuration.integrate && inner.receiver_configuration.integrate {
            info!("Stopping integration");
            inner.receiver_configuration.integrate = false;
        }
        Ok(inner.receiver_configuration)
    }

    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
        let inner = self.inner.lock().await;

        let (status, commanded_horizontal) = if let Some(target) = inner.target {
            let raw =
                calculate_target_horizontal(inner.location, Utc::now(), target, &inner.tle_cache)
                    .unwrap_or(Direction {
                        azimuth: 0.0,
                        elevation: -1.0,
                    });
            let target_horizontal = apply_offset(raw, inner.az_offset_rad, inner.el_offset_rad);
            let horizontal_offset_squared = (target_horizontal.azimuth - inner.horizontal.azimuth)
                .powi(2)
                + (target_horizontal.elevation - inner.horizontal.elevation).powi(2);
            let status = if horizontal_offset_squared > 0.2f64.to_radians().powi(2) {
                TelescopeStatus::Slewing
            } else {
                TelescopeStatus::Tracking
            };
            (status, Some(target_horizontal))
        } else {
            (TelescopeStatus::Idle, None)
        };

        let latest_observation = if inner.current_spectra.is_empty() {
            None
        } else {
            let mut latest_observation = ObservedSpectra {
                frequencies: vec![0f64; FAKE_TELESCOPE_CHANNELS],
                spectra: vec![0f64; FAKE_TELESCOPE_CHANNELS],
                observation_time: Duration::from_secs(0),
            };
            for integration in &inner.current_spectra {
                latest_observation.spectra = latest_observation
                    .spectra
                    .into_iter()
                    .zip(integration.spectra.iter())
                    .map(|(a, b)| a + b)
                    .collect();
                latest_observation.observation_time += integration.observation_time;
            }
            latest_observation.frequencies = inner.current_spectra[0].frequencies.clone();
            latest_observation.spectra = latest_observation
                .spectra
                .into_iter()
                .map(|value| value / inner.current_spectra.len() as f64)
                .collect();
            Some(latest_observation)
        };
        Ok(TelescopeInfo {
            id: inner.name.clone(),
            status,
            current_horizontal: Some(inner.horizontal),
            commanded_horizontal,
            current_target: inner.target,
            most_recent_error: inner.most_recent_error.clone(),
            measurement_in_progress: inner.receiver_configuration.integrate,
            latest_observation,
            stow_position: inner.stow_position,
            az_offset_rad: inner.az_offset_rad,
            el_offset_rad: inner.el_offset_rad,
            location: inner.location,
            min_elevation_rad: inner.min_elevation_rad,
            max_elevation_rad: inner.max_elevation_rad,
            webcam_crop: inner.webcam_crop,
            receiver_reachable: None,
        })
    }
    async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        inner.alive = false;
        debug!("Shutting down {}", inner.name);
    }
}

impl Inner {
    fn update(&mut self, delta_time: Duration) -> Result<(), TelescopeError> {
        assert!(self.alive);

        if let Some(target) = self.target {
            let now = Utc::now();
            let current_horizontal = self.horizontal;
            let Some(raw) =
                calculate_target_horizontal(self.location, now, target, &self.tle_cache)
            else {
                // Satellite not yet in TLE cache — skip update
                return Ok(());
            };
            let target_horizontal = apply_offset(raw, self.az_offset_rad, self.el_offset_rad);

            if target_horizontal.elevation < self.min_elevation_rad
                || target_horizontal.elevation > self.max_elevation_rad
            {
                info!(
                    "Stopping telescope since target {:?} is out of elevation range.",
                    &target
                );
                self.most_recent_error = Some(TelescopeError::TargetOutOfElevationRange {
                    min_deg: self.min_elevation_rad.to_degrees(),
                    max_deg: self.max_elevation_rad.to_degrees(),
                });
            } else {
                let max_delta_angle = FAKE_TELESCOPE_SLEWING_SPEED * delta_time.as_secs_f64();
                self.horizontal.azimuth += (target_horizontal.azimuth - current_horizontal.azimuth)
                    .clamp(-max_delta_angle, max_delta_angle);
                self.horizontal.elevation += (target_horizontal.elevation
                    - current_horizontal.elevation)
                    .clamp(-max_delta_angle, max_delta_angle);
            }
        }

        if self.receiver_configuration.integrate {
            trace!("Pushing spectum...");
            self.current_spectra.push(create_fake_spectra(delta_time))
        }

        Ok(())
    }
}

fn create_fake_spectra(integration_time: Duration) -> ObservedSpectra {
    let mut rng = rand::rng();

    let frequencies: Vec<f64> = (0..FAKE_TELESCOPE_CHANNELS)
        .map(|channel| channel as f64 * FAKE_TELESCOPE_CHANNEL_WIDTH + FAKE_TELESCOPE_FIRST_CHANNEL)
        .collect();
    let spectra: Vec<f64> = vec![5f64; FAKE_TELESCOPE_CHANNELS]
        .into_iter()
        .map(|value| {
            value + FAKE_TELESCOPE_NOISE * rng.sample::<f64, StandardNormal>(StandardNormal)
        })
        .collect();

    ObservedSpectra {
        frequencies,
        spectra,
        observation_time: integration_time,
    }
}

fn calculate_target_horizontal(
    location: Location,
    when: DateTime<Utc>,
    target: TelescopeTarget,
    tle_cache: &TleCacheHandle,
) -> Option<Direction> {
    match target {
        TelescopeTarget::Equatorial {
            right_ascension: ra,
            declination: dec,
        } => Some(horizontal_from_equatorial(location, when, ra, dec)),
        TelescopeTarget::Galactic {
            longitude: l,
            latitude: b,
        } => Some(horizontal_from_galactic(location, when, l, b)),
        TelescopeTarget::Horizontal {
            azimuth: az,
            elevation: el,
        } => Some(Direction {
            azimuth: az,
            elevation: el,
        }),
        TelescopeTarget::Sun => Some(horizontal_from_sun(location, when)),
        TelescopeTarget::Satellite { norad_id } => {
            tle_cache.satellite_direction(norad_id, location, when)
        }
    }
}

fn apply_offset(dir: Direction, az_offset_rad: f64, el_offset_rad: f64) -> Direction {
    let full_circle = 2.0 * PI;
    Direction {
        azimuth: ((dir.azimuth + az_offset_rad) % full_circle + full_circle) % full_circle,
        elevation: dir.elevation + el_offset_rad,
    }
}
