use crate::coords::{Direction, Location, horizontal_from_elements};
use chrono::{DateTime, Utc};
use std::sync::{Arc, RwLock};
use tokio::time::{Duration, interval};

const TLE_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CELESTRAK_URL: &str = "https://celestrak.org/NORAD/elements/gp.php";
const CELESTRAK_GROUPS: &[&str] = &["gps-ops", "galileo", "glo-ops", "beidou"];

pub struct SatelliteInfo {
    pub norad_id: u64,
    pub name: String,
    pub direction: Direction,
    pub freq_mhz: f64,
}

#[derive(Clone)]
pub struct TleCacheHandle {
    elements: Arc<RwLock<Vec<sgp4::Elements>>>,
}

impl TleCacheHandle {
    pub fn new() -> Self {
        TleCacheHandle {
            elements: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn visible_satellites(
        &self,
        location: Location,
        when: DateTime<Utc>,
    ) -> Vec<SatelliteInfo> {
        let elements = self.elements.read().unwrap();
        let mut satellites: Vec<SatelliteInfo> = elements
            .iter()
            .filter_map(|el| {
                let dir = horizontal_from_elements(el, location, when)?;
                if dir.elevation <= 0.0 {
                    return None;
                }
                let name = el
                    .object_name
                    .as_deref()
                    .unwrap_or("UNKNOWN")
                    .trim()
                    .to_string();
                let freq_mhz = gnss_freq_mhz(&name);
                Some(SatelliteInfo {
                    norad_id: el.norad_id,
                    name,
                    direction: dir,
                    freq_mhz,
                })
            })
            .collect();
        // Sort by elevation descending so best satellites appear first
        satellites.sort_by(|a, b| {
            b.direction
                .elevation
                .partial_cmp(&a.direction.elevation)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        satellites
    }

    pub fn satellite_direction(
        &self,
        norad_id: u64,
        location: Location,
        when: DateTime<Utc>,
    ) -> Option<Direction> {
        let elements = self.elements.read().unwrap();
        elements
            .iter()
            .find(|el| el.norad_id == norad_id)
            .and_then(|el| horizontal_from_elements(el, location, when))
    }

    pub fn satellite_name(&self, norad_id: u64) -> Option<String> {
        let elements = self.elements.read().unwrap();
        elements
            .iter()
            .find(|el| el.norad_id == norad_id)
            .and_then(|el| el.object_name.as_deref().map(|s| s.trim().to_string()))
    }

    pub fn is_empty(&self) -> bool {
        self.elements.read().unwrap().is_empty()
    }
}

/// Estimate primary L-band frequency for a GNSS satellite by name.
pub fn gnss_freq_mhz(name: &str) -> f64 {
    let upper = name.to_uppercase();
    if upper.starts_with("COSMOS") {
        // GLONASS: G1 band center
        1602.0
    } else if upper.starts_with("BEIDOU") {
        // BeiDou: B1I
        1561.098
    } else {
        // GPS (PRN) and Galileo (GSAT): L1/E1
        1575.42
    }
}

async fn fetch_elements(client: &reqwest::Client) -> Vec<sgp4::Elements> {
    let mut all = Vec::new();
    for group in CELESTRAK_GROUPS {
        match client
            .get(CELESTRAK_URL)
            .query(&[("GROUP", *group), ("FORMAT", "json")])
            .send()
            .await
        {
            Ok(resp) => match resp.json::<Vec<sgp4::Elements>>().await {
                Ok(elements) => {
                    log::info!(
                        "TLE: fetched {} elements for group {}",
                        elements.len(),
                        group
                    );
                    all.extend(elements);
                }
                Err(e) => log::error!("TLE: failed to parse group {}: {}", group, e),
            },
            Err(e) => log::error!("TLE: failed to fetch group {}: {}", group, e),
        }
    }
    all
}

pub fn start_tle_refresh(cache: TleCacheHandle) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        // First tick fires immediately, then every 24h
        let mut ticker = interval(TLE_REFRESH_INTERVAL);
        loop {
            ticker.tick().await;
            let elements = fetch_elements(&client).await;
            if !elements.is_empty() {
                let n = elements.len();
                *cache.elements.write().unwrap() = elements;
                log::info!("TLE cache updated: {} satellites total", n);
            }
        }
    });
}
