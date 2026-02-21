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
    Parked, // aka Stow
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
    pub current_target: TelescopeTarget,
    pub most_recent_error: Option<TelescopeError>,
    pub measurement_in_progress: bool,
    pub latest_observation: Option<ObservedSpectra>,
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
    pub telescope_type: TelescopeType,
    pub controller_address: Option<String>,
    pub receiver_address: Option<String>,
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

#[derive(Serialize, Deserialize, PartialEq, Debug, Copy, Clone)]
pub struct ReceiverConfiguration {
    pub integrate: bool,
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
