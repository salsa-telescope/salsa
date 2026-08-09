use crate::coords::{Direction, Location};
use crate::models::telescope_types::{
    CalibrationResult, IqBlock, ObservedSpectra, ReceiverConfiguration, ReceiverError,
    TelescopeDefinition, TelescopeError, TelescopeInfo, TelescopeTarget, TelescopeType,
    TelescopesConfig,
};

use crate::models::fake_telescope;
use crate::models::salsa_telescope;
use crate::tle_cache::TleCacheHandle;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[async_trait]
pub trait Telescope: Send + Sync {
    async fn set_target(
        &self,
        target: TelescopeTarget,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError>;
    async fn stop(&self) -> Result<(), TelescopeError>;
    /// Correct a measured pointing offset by rewriting the rotor
    /// controller's stored current position (without moving the rotor).
    /// The offsets are the observing offsets at which the peak of a strong
    /// source was found; the reported position decreases by these amounts.
    /// Refused while a target is being tracked.
    async fn calibrate(
        &self,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<CalibrationResult, TelescopeError>;
    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError>;
    /// Stop integration, wait for any in-progress data recording to finish, and return the
    /// accumulated spectra. Returns None if integration was not running. Calling this twice
    /// always returns None on the second call, preventing double-saves.
    async fn stop_integration(&self) -> Option<ObservedSpectra>;
    /// Drop the in-memory cache of the most recent spectrum so the next user
    /// of this telescope does not see the previous user's data on the live
    /// page. Safe to call when no integration is active; idempotent.
    async fn clear_measurements(&self);
    /// Whether this telescope can participate in interferometry sessions.
    /// Real (Salsa) telescopes need an external 10 MHz / PPS reference (GPSDO
    /// enabled) so that the two N210s' sample clocks align — without it the
    /// correlator cannot match A/B blocks. Fake telescopes synthesise aligned
    /// timestamps internally and are always capable.
    async fn interferometry_capable(&self) -> bool;
    /// A clone of the current integration's cancellation token, or `None` if
    /// no integration is in flight. Used by the fixed-duration auto-stop
    /// task to bail out cleanly when the integration it was started for
    /// has already been stopped (manually or by another mechanism).
    async fn current_integration_token(&self) -> Option<tokio_util::sync::CancellationToken>;
    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError>;
    async fn shutdown(&self);
    /// Start streaming raw IQ blocks for interferometry correlation.
    /// Each block carries a timestamp (seconds, relative to USRP time zero for
    /// real hardware, or stream start for the fake telescope) so the correlator
    /// can align A- and B-side blocks.
    /// Stop by calling set_receiver_configuration(integrate: false).
    async fn start_iq_stream(
        &self,
        config: ReceiverConfiguration,
    ) -> Result<tokio::sync::mpsc::Receiver<IqBlock>, ReceiverError>;
}

type TelescopeCollection = Arc<RwLock<HashMap<String, Arc<dyn Telescope>>>>;

// Hide all synchronization for handling telescopes inside this type. Exposes an
// async api without any client-visible locks for managing the collection of
// telescopes.
#[derive(Clone)]
pub struct TelescopeCollectionHandle {
    telescopes: TelescopeCollection,
}

impl TelescopeCollectionHandle {
    pub async fn get(&self, id: &str) -> Option<Arc<dyn Telescope>> {
        let telescopes = self.telescopes.read().await;
        telescopes.get(id).cloned()
    }

    pub async fn get_all(&self) -> Vec<Arc<dyn Telescope>> {
        let telescopes = self.telescopes.read().await;
        telescopes.values().cloned().collect()
    }

    pub async fn contains_key(&self, id: &str) -> bool {
        let telescopes = self.telescopes.read().await;
        telescopes.contains_key(id)
    }

    /// Telescope names in [`DISPLAY_ORDER`]. Every page that lists the
    /// telescopes uses this, so they all agree on the order.
    pub async fn get_names(&self) -> Vec<String> {
        let telescopes = self.telescopes.read().await;
        let mut res: Vec<_> = telescopes.keys().cloned().collect();
        res.sort();
        sort_by_preference(&mut res, &DISPLAY_ORDER);
        res
    }
}

/// The order the telescopes stand in, left to right, as the webcam sees them.
/// The live page lists its status cards in this order so they line up with the
/// dishes in the panorama above them, and the booking calendar uses the same
/// order for its columns. It only changes if a telescope is physically moved.
/// Anything not named here — the fake telescopes, or a newly added dish —
/// follows, in alphabetical order.
///
/// Which telescope a guest is handed first is a separate question, and a
/// policy rather than a physical one: see `GuestConfig::telescope_priority`.
const DISPLAY_ORDER: [&str; 3] = ["torre", "vale", "brage"];

/// Move the names listed in `preference` to the front of `names`, in the order
/// `preference` gives them. Matching is case-insensitive, and a preferred name
/// that no telescope has is simply ignored.
///
/// The sort is stable, so names the list does not mention keep the order they
/// arrived in — an empty `preference` leaves `names` untouched.
pub fn sort_by_preference(names: &mut [String], preference: &[impl AsRef<str>]) {
    names.sort_by_key(|name| {
        preference
            .iter()
            .position(|p| p.as_ref().eq_ignore_ascii_case(name))
            .unwrap_or(usize::MAX)
    });
}

fn create_telescope(def: TelescopeDefinition, tle_cache: TleCacheHandle) -> Arc<dyn Telescope> {
    info!("Creating telescope {}", def.name);
    let location = Location {
        longitude: def.location[0].to_radians(),
        latitude: def.location[1].to_radians(),
    };
    let stow_position = def.stow_position.map(|p| Direction {
        azimuth: p[0].to_radians(),
        elevation: p[1].to_radians(),
    });
    let service_position = def.service_position.map(|p| Direction {
        azimuth: p[0].to_radians(),
        elevation: p[1].to_radians(),
    });
    let min_elevation_rad = def.min_elevation.to_radians();
    let max_elevation_rad = def.max_elevation.to_radians();

    // The tracker rejects any move outside the elevation limits, including
    // these fixed positions. Catching it here names the offending config key;
    // otherwise the first operator to select the position gets a bare "target
    // out of elevation range" and no hint that it can never succeed.
    for (name, position) in [("stow", stow_position), ("service", service_position)] {
        if let Some(position) = position
            && (position.elevation < min_elevation_rad || position.elevation > max_elevation_rad)
        {
            warn!(
                "Telescope {}: {name}_position elevation {:.1}° is outside the configured \
                 elevation range {:.1}°–{:.1}°, so moving there will always be refused",
                def.name,
                position.elevation.to_degrees(),
                def.min_elevation,
                def.max_elevation
            );
        }
    }

    let default_ref_freq_hz = def.default_ref_freq_mhz * 1e6;
    let default_gain_db = def.default_gain_db;
    let tsys_k = def.tsys_k;
    match def.telescope_type {
        TelescopeType::Salsa => Arc::new(salsa_telescope::create(
            def.name.clone(),
            def.controller_address
                .expect("Telescope of type Salsa should have controller_address.")
                .clone(),
            def.receiver_address
                .expect("Telescope of type Salsa should have receiver_address.")
                .clone(),
            def.gpsdo_enabled,
            stow_position,
            service_position,
            location,
            min_elevation_rad,
            max_elevation_rad,
            def.webcam_crop,
            default_ref_freq_hz,
            default_gain_db,
            tsys_k,
            def.wind_warning_ms,
            tle_cache,
        )),
        TelescopeType::Fake => Arc::new(fake_telescope::create(
            def.name.clone(),
            stow_position,
            service_position,
            location,
            min_elevation_rad,
            max_elevation_rad,
            def.webcam_crop,
            default_ref_freq_hz,
            default_gain_db,
            tle_cache,
        )),
    }
}

pub fn create_telescope_collection(
    config_filepath: impl Into<PathBuf>,
    tle_cache: TleCacheHandle,
) -> TelescopeCollectionHandle {
    let config: TelescopesConfig =
        toml::from_str(&fs::read_to_string(config_filepath.into()).unwrap_or_default())
            .expect("telescope config file should be valid toml.");
    let telescopes: HashMap<_, _> = config
        .telescopes
        .into_iter()
        .map(|telescope_definition| {
            (
                telescope_definition.name.clone(),
                create_telescope(telescope_definition, tle_cache.clone()),
            )
        })
        .collect();

    TelescopeCollectionHandle {
        telescopes: Arc::new(RwLock::new(telescopes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn preferred_names_come_first_in_the_order_given() {
        let mut telescopes = names(&["torre", "vale", "brage"]);
        sort_by_preference(&mut telescopes, &["vale", "brage", "torre"]);
        assert_eq!(telescopes, names(&["vale", "brage", "torre"]));
    }

    #[test]
    fn unpreferred_names_keep_their_incoming_order() {
        // The guest priority sorts a list that is already in display order,
        // so anything it does not mention has to stay where it was.
        let mut telescopes = names(&["torre", "vale", "brage", "fake1", "fake2"]);
        sort_by_preference(&mut telescopes, &["vale"]);
        assert_eq!(
            telescopes,
            names(&["vale", "torre", "brage", "fake1", "fake2"])
        );
    }

    #[test]
    fn empty_preference_changes_nothing() {
        let mut telescopes = names(&["torre", "vale", "brage"]);
        sort_by_preference(&mut telescopes, &[] as &[String]);
        assert_eq!(telescopes, names(&["torre", "vale", "brage"]));
    }

    #[test]
    fn preference_matching_ignores_case() {
        let mut telescopes = names(&["torre", "Vale"]);
        sort_by_preference(&mut telescopes, &["VALE"]);
        assert_eq!(telescopes, names(&["Vale", "torre"]));
    }

    #[test]
    fn unknown_preferred_names_are_ignored() {
        let mut telescopes = names(&["torre", "vale", "brage"]);
        sort_by_preference(&mut telescopes, &["nosuchdish", "brage"]);
        assert_eq!(telescopes, names(&["brage", "torre", "vale"]));
    }
}
