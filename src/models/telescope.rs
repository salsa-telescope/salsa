use crate::coords::Direction;
use crate::models::telescope_types::{
    ReceiverConfiguration, ReceiverError, TelescopeDefinition, TelescopeError, TelescopeInfo,
    TelescopeTarget, TelescopeType, TelescopesConfig,
};

use crate::models::fake_telescope;
use crate::models::salsa_telescope;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[async_trait]
pub trait Telescope: Send + Sync {
    async fn get_direction(&self) -> Result<Direction, TelescopeError>;
    async fn set_target(&self, target: TelescopeTarget) -> Result<TelescopeTarget, TelescopeError>;
    async fn set_receiver_configuration(
        &self,
        receiver_configuration: ReceiverConfiguration,
    ) -> Result<ReceiverConfiguration, ReceiverError>;
    async fn get_info(&self) -> Result<TelescopeInfo, TelescopeError>;
}

type TelescopeCollection = Arc<RwLock<HashMap<String, Arc<dyn Telescope>>>>;

// Hide all synchronization for handling telescopes inside this type. Exposes an
// async api without any client-visible locks for managing the collection of
// telescopes.
#[derive(Clone)]
pub struct TelescopeCollectionHandle {
    telescopes: TelescopeCollection,
    names: Vec<String>,
}

impl TelescopeCollectionHandle {
    pub async fn get(&self, id: &str) -> Option<Arc<dyn Telescope>> {
        let telescopes_read_lock = self.telescopes.read().await;
        telescopes_read_lock.get(id).cloned()
    }

    pub async fn contains_key(&self, id: &str) -> bool {
        let telescopes_read_lock = self.telescopes.read().await;
        telescopes_read_lock.contains_key(id)
    }

    pub fn get_names(&self) -> Vec<String> {
        self.names.clone()
    }
}

fn create_telescope(def: TelescopeDefinition) -> Arc<dyn Telescope> {
    log::info!("Creating telescope {}", def.name);
    match def.telescope_type {
        TelescopeType::Salsa => Arc::new(salsa_telescope::create(
            def.name.clone(),
            def.controller_address
                .expect("Telescope of type Salsa should have controller_address.")
                .clone(),
            def.receiver_address
                .expect("Telescope of type Salsa should have receiver_address.")
                .clone(),
        )),
        TelescopeType::Fake => Arc::new(fake_telescope::create(def.name.clone())),
    }
}

pub fn create_telescope_collection(
    config_filepath: impl Into<PathBuf>,
) -> TelescopeCollectionHandle {
    let config: TelescopesConfig = toml::from_str(
        &fs::read_to_string(config_filepath.into())
            .expect("telescopes config file should exist and be readable."),
    )
    .expect("telescope config file should be valid toml.");
    let names = config
        .telescopes
        .iter()
        .map(|def| def.name.clone())
        .collect();
    let telescopes: HashMap<_, _> = config
        .telescopes
        .into_iter()
        .map(|telescope_definition| {
            (
                telescope_definition.name.clone(),
                create_telescope(telescope_definition),
            )
        })
        .collect();

    TelescopeCollectionHandle {
        telescopes: Arc::new(RwLock::new(telescopes)),
        names,
    }
}
