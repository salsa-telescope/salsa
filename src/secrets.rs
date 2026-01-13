use crate::error::InternalError;
use serde::Deserialize;
use std::{collections::HashMap, fs::read_to_string};

#[derive(Deserialize, Clone)]
pub struct AuthProvider {
    pub auth_uri: String,
    pub token_uri: String,
    pub redirect_uri: String,
    pub user_uri: String,
    pub display_name_field: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize)]
pub struct Secrets {
    auth_provider: HashMap<String, AuthProvider>,
}

impl Secrets {
    pub fn read(filename: &str) -> Result<Secrets, InternalError> {
        let contents = read_to_string(filename).map_err(|err| {
            InternalError::new(format!("Failed to read from '{filename}': {err}"))
        })?;
        let secrets: Secrets = toml::from_str(&contents).map_err(|err| {
            InternalError::new(format!("Failed to parse toml from {filename}: {err}"))
        })?;
        Ok(secrets)
    }

    pub fn get_auth_provider_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.auth_provider.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn get_auth_provider(&self, provider_name: &str) -> Result<AuthProvider, InternalError> {
        let Some(auth_provider) = self.auth_provider.get(provider_name) else {
            return Err(InternalError::new(format!(
                "No provider with name {provider_name} configured"
            )));
        };
        Ok(auth_provider.clone())
    }
}
