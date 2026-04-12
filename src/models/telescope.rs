use crate::coords::{Direction, Location};
use crate::models::telescope_types::{
    ObservedSpectra, ReceiverConfiguration, ReceiverError, TelescopeDefinition, TelescopeError,
    TelescopeInfo, TelescopeTarget, TelescopeType, TelescopesConfig,
};

use crate::models::fake_telescope;
use crate::models::salsa_telescope;
use crate::tle_cache::TleCacheHandle;
use crate::weather_cache::WeatherCacheHandle;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[async_trait]
pub trait Telescope: Send + Sync {
    async fn set_target(
        &self,
        target: TelescopeTarget,
        az_offset_rad: f64,
        el_offset_rad: f64,
    ) -> Result<TelescopeTarget, TelescopeError>;
    async fn stop(&self) -> Result<(), TelescopeError>;
    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError>;
    /// Stop integration, wait for any in-progress data recording to finish, and return the
    /// accumulated spectra. Returns None if integration was not running. Calling this twice
    /// always returns None on the second call, preventing double-saves.
    async fn stop_integration(&self) -> Option<ObservedSpectra>;
    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError>;
    async fn shutdown(&self);
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

    pub async fn get_names(&self) -> Vec<String> {
        let telescopes = self.telescopes.read().await;
        let mut res: Vec<_> = telescopes.keys().cloned().collect();
        res.sort();
        res
    }
}

fn create_telescope(
    def: TelescopeDefinition,
    tle_cache: TleCacheHandle,
    weather_cache: WeatherCacheHandle,
) -> Arc<dyn Telescope> {
    info!("Creating telescope {}", def.name);
    let location = Location {
        longitude: def.location[0].to_radians(),
        latitude: def.location[1].to_radians(),
    };
    let stow_position = def.stow_position.map(|p| Direction {
        azimuth: p[0].to_radians(),
        elevation: p[1].to_radians(),
    });
    let min_elevation_rad = def.min_elevation.to_radians();
    let max_elevation_rad = def.max_elevation.to_radians();
    let default_ref_freq_hz = def.default_ref_freq_mhz * 1e6;
    let default_gain_db = def.default_gain_db;
    let t_rec_k = def.t_rec_k;
    match def.telescope_type {
        TelescopeType::Salsa => Arc::new(salsa_telescope::create(
            def.name.clone(),
            def.controller_address
                .expect("Telescope of type Salsa should have controller_address.")
                .clone(),
            def.receiver_address
                .expect("Telescope of type Salsa should have receiver_address.")
                .clone(),
            stow_position,
            location,
            min_elevation_rad,
            max_elevation_rad,
            def.webcam_crop,
            default_ref_freq_hz,
            default_gain_db,
            t_rec_k,
            def.wind_warning_ms,
            tle_cache,
            weather_cache,
        )),
        TelescopeType::Fake => Arc::new(fake_telescope::create(
            def.name.clone(),
            stow_position,
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
    weather_cache: WeatherCacheHandle,
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
                create_telescope(
                    telescope_definition,
                    tle_cache.clone(),
                    weather_cache.clone(),
                ),
            )
        })
        .collect();

    TelescopeCollectionHandle {
        telescopes: Arc::new(RwLock::new(telescopes)),
    }
}
