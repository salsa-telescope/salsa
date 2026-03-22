use crate::coords::{Direction, Location};
use chrono::{DateTime, offset::Utc};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::time::Duration;

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone)]
pub enum TelescopeTarget {
    Equatorial {
        right_ascension: f64, // in radians
        declination: f64,     // in radians
    },
    Galactic {
        longitude: f64, // in radians
        latitude: f64,  // in radians
    },
    Horizontal {
        azimuth: f64,   // in radians
        elevation: f64, // in radians
    },
    Sun,
    Satellite {
        norad_id: u64,
    },
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone)]
pub enum TelescopeStatus {
    Idle,
    Slewing,
    Tracking,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct ObservedSpectra {
    pub frequencies: Vec<f64>,
    pub spectra: Vec<f64>,
    pub observation_time: Duration,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct TelescopeInfo {
    pub id: String,
    pub status: TelescopeStatus,
    pub commanded_horizontal: Option<Direction>,
    pub current_horizontal: Option<Direction>,
    pub current_target: Option<TelescopeTarget>,
    pub most_recent_error: Option<TelescopeError>,
    pub measurement_in_progress: bool,
    pub latest_observation: Option<ObservedSpectra>,
    pub stow_position: Option<Direction>,
    pub az_offset_rad: f64,
    pub el_offset_rad: f64,
}

#[derive(Deserialize, PartialEq, Debug, Clone)]
pub enum TelescopeType {
    Salsa,
    Fake,
}

#[derive(Deserialize, PartialEq, Debug, Clone)]
pub struct TelescopeDefinition {
    pub name: String,
    pub enabled: bool,
    pub location: Location,
    pub min_elevation: f64,
    pub stow_position: Option<[f64; 2]>, // [azimuth, elevation] in degrees
    pub telescope_type: TelescopeType,
    pub controller_address: Option<String>,
    pub receiver_address: Option<String>,
    #[serde(default = "default_ref_freq_mhz")]
    pub default_ref_freq_mhz: f64, // default reference frequency in MHz (for freq-switched mode)
    #[serde(default = "default_gain_db")]
    pub default_gain_db: f64, // default receiver gain in dB
}

#[derive(Deserialize, PartialEq, Debug, Clone)]
pub struct TelescopesConfig {
    pub telescopes: Vec<TelescopeDefinition>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum TelescopeError {
    TargetBelowHorizon,
    TelescopeIOError(String),
    TelescopeNotConnected,
}

impl Display for TelescopeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TelescopeError::TargetBelowHorizon => {
                f.write_str("Failed to set target, target is below horizon.")
            }
            TelescopeError::TelescopeIOError(message) => f.write_str(&format!(
                "Error in communication with telescope: {}",
                message
            )),
            TelescopeError::TelescopeNotConnected => f.write_str("Telescope is not connected."),
        }
    }
}

impl From<std::io::Error> for TelescopeError {
    fn from(error: std::io::Error) -> Self {
        TelescopeError::TelescopeIOError(format!("Communication with telescope failed: {}", error))
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone)]
pub enum ReceiverError {
    IntegrationAlreadyRunning,
}

impl Display for ReceiverError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiverError::IntegrationAlreadyRunning => f.write_str("Integration already running"),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone, Default)]
pub enum ObservationMode {
    #[default]
    FreqSwitched,
    Raw,
}

fn default_center_freq_hz() -> f64 {
    1.4204e9
}

fn default_ref_freq_hz() -> f64 {
    1.4179e9
}

fn default_ref_freq_mhz() -> f64 {
    default_ref_freq_hz() / 1e6
}

fn default_bandwidth_hz() -> f64 {
    2.5e6
}

fn default_gain_db() -> f64 {
    60.0
}

fn default_spectral_channels() -> usize {
    512
}

fn default_rfi_filter() -> bool {
    true
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone)]
pub struct ReceiverConfiguration {
    pub integrate: bool,
    #[serde(default)]
    pub mode: ObservationMode,
    #[serde(default = "default_center_freq_hz")]
    pub center_freq_hz: f64,
    #[serde(default = "default_ref_freq_hz")]
    pub ref_freq_hz: f64,
    #[serde(default = "default_bandwidth_hz")]
    pub bandwidth_hz: f64,
    #[serde(default = "default_gain_db")]
    pub gain_db: f64,
    #[serde(default = "default_spectral_channels")]
    pub spectral_channels: usize,
    #[serde(default = "default_rfi_filter")]
    pub rfi_filter: bool,
}

impl Default for ReceiverConfiguration {
    fn default() -> Self {
        ReceiverConfiguration {
            integrate: false,
            mode: ObservationMode::default(),
            center_freq_hz: default_center_freq_hz(),
            ref_freq_hz: default_ref_freq_hz(),
            bandwidth_hz: default_bandwidth_hz(),
            gain_db: default_gain_db(),
            spectral_channels: default_spectral_channels(),
            rfi_filter: default_rfi_filter(),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Measurement {
    pub amps: Vec<f64>,
    pub freqs: Vec<f64>,
    //glon: f64,
    //glat: f64,
    pub start: DateTime<Utc>,
    pub duration: Duration,
    //stop: Option<DateTime<Utc>>,
    //vlsr_correction: Option<f64>,
    //telname: String,
    //tellat: f64,
    //tellon: f64,
}
