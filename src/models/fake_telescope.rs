use crate::coords::{Direction, Location};
use crate::coords::{horizontal_from_equatorial, horizontal_from_galactic};
use crate::models::telescope::Telescope;
use crate::models::telescope_types::{
    ObservedSpectra, ReceiverConfiguration, ReceiverError, TelescopeError, TelescopeInfo,
    TelescopeStatus, TelescopeTarget,
};
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
pub const LOWEST_ALLOWED_ELEVATION: f64 = 5.0 / 180. * PI;

pub const FAKE_TELESCOPE_SLEWING_SPEED: f64 = PI / 10.0;
pub const FAKE_TELESCOPE_CHANNELS: usize = 400;
pub const FAKE_TELESCOPE_CHANNEL_WIDTH: f64 = 2e6f64 / FAKE_TELESCOPE_CHANNELS as f64;
pub const FAKE_TELESCOPE_FIRST_CHANNEL: f64 =
    1.420e9f64 - FAKE_TELESCOPE_CHANNEL_WIDTH * FAKE_TELESCOPE_CHANNELS as f64 / 2f64;
pub const FAKE_TELESCOPE_NOISE: f64 = 2f64;
pub const TELESCOPE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

struct Inner {
    target: TelescopeTarget,
    horizontal: Direction,
    location: Location,
    most_recent_error: Option<TelescopeError>,
    receiver_configuration: ReceiverConfiguration,
    current_spectra: Vec<ObservedSpectra>,
    name: String,
    alive: bool,
}

pub struct FakeTelescope {
    inner: Arc<Mutex<Inner>>,
}

pub fn create(name: String) -> FakeTelescope {
    let inner = Arc::new(Mutex::new(Inner {
        target: TelescopeTarget::Parked,
        horizontal: FAKE_TELESCOPE_PARKING_HORIZONTAL,
        location: Location {
            //(11.0+55.0/60.0+7.5/3600.0) * PI / 180.0. Sign positive, handled in gmst calc
            longitude: 0.20802143022,
            //(57.0+23.0/60.0+36.4/3600.0) * PI / 180.0
            latitude: 1.00170457462,
        },
        most_recent_error: None,
        receiver_configuration: ReceiverConfiguration { integrate: false },
        current_spectra: vec![],
        name,
        alive: true,
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
    async fn get_direction(&self) -> Option<Direction> {
        let inner = self.inner.lock().await;
        Some(inner.horizontal)
    }

    async fn set_target(&self, target: TelescopeTarget) -> Result<TelescopeTarget, TelescopeError> {
        let mut inner = self.inner.lock().await;

        inner.most_recent_error = None;
        inner.receiver_configuration.integrate = false;
        inner.current_spectra.clear();

        let target_horizontal = calculate_target_horizontal(inner.location, Utc::now(), target);
        if target_horizontal.elevation < LOWEST_ALLOWED_ELEVATION {
            info!(
                "Refusing to set target for telescope {} to {:?}. Target is below horizon",
                &inner.name, &target
            );
            Err(TelescopeError::TargetBelowHorizon)
        } else {
            info!(
                "Setting target for telescope {} to {:?}",
                &inner.name, &target
            );
            inner.target = target;
            Ok(target)
        }
    }

    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError> {
        let mut inner = self.inner.lock().await;

        if receiver_configuration.integrate && !inner.receiver_configuration.integrate {
            info!("Starting integration");
            inner.receiver_configuration.integrate = true;
        } else if !receiver_configuration.integrate && inner.receiver_configuration.integrate {
            info!("Stopping integration");
            inner.receiver_configuration.integrate = false;
        }
        Ok(inner.receiver_configuration)
    }

    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError> {
        let inner = self.inner.lock().await;

        let target_horizontal =
            calculate_target_horizontal(inner.location, Utc::now(), inner.target);

        let horizontal_offset_squared = (target_horizontal.azimuth - inner.horizontal.azimuth)
            .powi(2)
            + (target_horizontal.elevation - inner.horizontal.elevation).powi(2);
        let status = {
            if horizontal_offset_squared > 0.2f64.to_radians().powi(2) {
                TelescopeStatus::Slewing
            } else if inner.target == TelescopeTarget::Parked {
                TelescopeStatus::Idle
            } else {
                TelescopeStatus::Tracking
            }
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
            commanded_horizontal: Some(target_horizontal),
            current_target: inner.target,
            most_recent_error: inner.most_recent_error.clone(),
            measurement_in_progress: inner.receiver_configuration.integrate,
            latest_observation,
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
        let now = Utc::now();
        let current_horizontal = self.horizontal;
        let target_horizontal = calculate_target_horizontal(self.location, now, self.target);

        if target_horizontal.elevation < LOWEST_ALLOWED_ELEVATION {
            info!(
                "Stopping telescope since target {:?} set below horizon.",
                &self.target
            );
            self.most_recent_error = Some(TelescopeError::TargetBelowHorizon);
        } else {
            let max_delta_angle = FAKE_TELESCOPE_SLEWING_SPEED * delta_time.as_secs_f64();
            self.horizontal.azimuth += (target_horizontal.azimuth - current_horizontal.azimuth)
                .clamp(-max_delta_angle, max_delta_angle);
            self.horizontal.elevation += (target_horizontal.elevation
                - current_horizontal.elevation)
                .clamp(-max_delta_angle, max_delta_angle);
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
) -> Direction {
    match target {
        TelescopeTarget::Equatorial {
            right_ascension: ra,
            declination: dec,
        } => horizontal_from_equatorial(location, when, ra, dec),
        TelescopeTarget::Galactic {
            longitude: l,
            latitude: b,
        } => horizontal_from_galactic(location, when, l, b),
        TelescopeTarget::Horizontal {
            azimuth: az,
            elevation: el,
        } => Direction {
            azimuth: az,
            elevation: el,
        },
        TelescopeTarget::Parked => FAKE_TELESCOPE_PARKING_HORIZONTAL,
    }
}
