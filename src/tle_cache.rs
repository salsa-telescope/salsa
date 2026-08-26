use crate::coords::{Direction, Location, horizontal_from_elements};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::time::Duration;
use tracing::{error, info, warn};

const TLE_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// First wait after a failed refresh cycle, doubled on each further failure up
/// to `TLE_RETRY_MAX`. Without this a single bad cycle cost a full day of
/// satellite tracking, because the loop simply slept until the next daily tick.
const TLE_RETRY_BASE: Duration = Duration::from_secs(60);
const TLE_RETRY_MAX: Duration = Duration::from_secs(60 * 60);
/// Celestrak rate-limits per IP, so the group requests are spaced out rather
/// than fired back to back.
const CELESTRAK_GROUP_DELAY: Duration = Duration::from_secs(2);
const TLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Elements older than this are reported as stale to the observer. GNSS orbits
/// are forgiving enough that a week of drift is far inside the beam — this is
/// a "the refresh has been broken for a while" signal, not a pointing limit.
const TLE_STALE_DAYS: i64 = 7;
const CELESTRAK_URL: &str = "https://celestrak.org/NORAD/elements/gp.php";
const CELESTRAK_GROUPS: &[&str] = &["gps-ops", "galileo", "glo-ops", "beidou"];
const CACHE_FILE: &str = "tle_cache.json";

pub struct SatelliteInfo {
    pub norad_id: u64,
    pub name: String,
    pub direction: Direction,
    pub freq_mhz: f64,
}

/// What the cache holds, as written to disk. Keeping the fetch time alongside
/// the elements distinguishes "we cannot reach Celestrak" from "Celestrak is
/// up but serving us old elements".
#[derive(Serialize, Deserialize, Clone)]
struct CachedTle {
    fetched_at: DateTime<Utc>,
    elements: Vec<sgp4::Elements>,
}

/// Freshness of the cached elements, for the satellite picker to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TleStatus {
    /// Nothing cached at all — the picker cannot offer any satellites.
    Unavailable,
    /// Cached, but the newest element is older than `TLE_STALE_DAYS`.
    Stale {
        age_days: i64,
    },
    Fresh {
        age_days: i64,
    },
}

impl TleStatus {
    /// Short tag for the JSON the satellite picker consumes.
    pub fn tag(&self) -> &'static str {
        match self {
            TleStatus::Unavailable => "unavailable",
            TleStatus::Stale { .. } => "stale",
            TleStatus::Fresh { .. } => "ok",
        }
    }

    pub fn age_days(&self) -> Option<i64> {
        match self {
            TleStatus::Unavailable => None,
            TleStatus::Stale { age_days } | TleStatus::Fresh { age_days } => Some(*age_days),
        }
    }
}

#[derive(Clone)]
pub struct TleCacheHandle {
    data: Arc<RwLock<Option<CachedTle>>>,
    /// Where to persist. `None` in tests, which keeps the cache in memory only.
    path: Option<PathBuf>,
}

impl Default for TleCacheHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl TleCacheHandle {
    pub fn new() -> Self {
        TleCacheHandle {
            data: Arc::new(RwLock::new(None)),
            path: None,
        }
    }

    /// Load any previously fetched elements from `database_dir`. Without this
    /// every restart began with an empty cache and needed a successful fetch
    /// before satellites worked at all, so a reboot during a Celestrak outage
    /// left the picker empty.
    pub fn with_persistence(database_dir: &Path) -> Self {
        let path = database_dir.join(CACHE_FILE);
        let data = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<CachedTle>(&bytes) {
                Ok(cached) => {
                    info!(
                        "TLE: loaded {} elements from {} (fetched {})",
                        cached.elements.len(),
                        path.display(),
                        cached.fetched_at
                    );
                    Some(cached)
                }
                Err(e) => {
                    warn!("TLE: ignoring unreadable cache {}: {}", path.display(), e);
                    None
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!("TLE: could not read cache {}: {}", path.display(), e);
                None
            }
        };
        TleCacheHandle {
            data: Arc::new(RwLock::new(data)),
            path: Some(path),
        }
    }

    /// Age of the freshest element we hold. The newest epoch is what says how
    /// current the dataset is: Celestrak issues each satellite its own epoch,
    /// so even a good fetch contains some elements a couple of days old.
    pub fn status(&self) -> TleStatus {
        let guard = self.data.read().unwrap();
        let Some(cached) = guard.as_ref() else {
            return TleStatus::Unavailable;
        };
        let Some(newest) = cached.elements.iter().map(|el| el.datetime).max() else {
            return TleStatus::Unavailable;
        };
        let age_days = (Utc::now().naive_utc() - newest).num_days().max(0);
        if age_days >= TLE_STALE_DAYS {
            TleStatus::Stale { age_days }
        } else {
            TleStatus::Fresh { age_days }
        }
    }

    fn store(&self, elements: Vec<sgp4::Elements>) {
        let cached = CachedTle {
            fetched_at: Utc::now(),
            elements,
        };
        if let Some(path) = &self.path {
            match serde_json::to_vec(&cached) {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(path, bytes) {
                        warn!("TLE: could not write cache {}: {}", path.display(), e);
                    }
                }
                Err(e) => warn!("TLE: could not serialise cache: {}", e),
            }
        }
        *self.data.write().unwrap() = Some(cached);
    }

    /// Run `f` over the cached elements, or over an empty slice when nothing
    /// is cached, so callers do not each have to unwrap the `Option`.
    fn with_elements<T>(&self, f: impl FnOnce(&[sgp4::Elements]) -> T) -> T {
        let guard = self.data.read().unwrap();
        match guard.as_ref() {
            Some(cached) => f(&cached.elements),
            None => f(&[]),
        }
    }

    pub fn visible_satellites(
        &self,
        location: Location,
        when: DateTime<Utc>,
    ) -> Vec<SatelliteInfo> {
        self.with_elements(|elements| {
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
        })
    }

    /// The satellite picker's payload: the visible satellites, plus how fresh
    /// the elements behind them are. The freshness travels with the list
    /// because an empty list on its own is ambiguous — it means "nothing is
    /// up right now" and "we have no elements at all" equally well, and the
    /// page needs to tell the observer which.
    pub fn satellites_json(&self, location: Location, when: DateTime<Utc>) -> serde_json::Value {
        let status = self.status();
        let satellites: Vec<_> = self
            .visible_satellites(location, when)
            .iter()
            .map(|s| {
                serde_json::json!({
                    "norad_id": s.norad_id,
                    "name": s.name,
                    "elevation_deg": s.direction.elevation.to_degrees(),
                    "azimuth_deg": s.direction.azimuth.to_degrees(),
                    "freq_mhz": s.freq_mhz,
                })
            })
            .collect();
        serde_json::json!({
            "satellites": satellites,
            "status": status.tag(),
            "age_days": status.age_days(),
        })
    }

    pub fn satellite_direction(
        &self,
        norad_id: u64,
        location: Location,
        when: DateTime<Utc>,
    ) -> Option<Direction> {
        self.with_elements(|elements| {
            elements
                .iter()
                .find(|el| el.norad_id == norad_id)
                .and_then(|el| horizontal_from_elements(el, location, when))
        })
    }

    pub fn satellite_name(&self, norad_id: u64) -> Option<String> {
        self.with_elements(|elements| {
            elements
                .iter()
                .find(|el| el.norad_id == norad_id)
                .and_then(|el| el.object_name.as_deref().map(|s| s.trim().to_string()))
        })
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

/// Fetch every group, or nothing. Returning whatever happened to succeed let
/// a single bad group be cached as if it were a full refresh: three good
/// groups reset the backoff and slept for a day, so one constellation could
/// quietly vanish from the picker with nothing stale enough to warn about.
async fn fetch_elements(client: &reqwest::Client) -> Option<Vec<sgp4::Elements>> {
    let mut all = Vec::new();
    let mut failed = 0;
    for (i, group) in CELESTRAK_GROUPS.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(CELESTRAK_GROUP_DELAY).await;
        }
        let response = client
            .get(CELESTRAK_URL)
            .query(&[("GROUP", *group), ("FORMAT", "json")])
            .send()
            .await
            // Celestrak answers an outage with an HTML error page rather than
            // JSON. Checking the status first means that shows up as the HTTP
            // error it is, instead of a misleading parse failure.
            .and_then(|resp| resp.error_for_status());
        match response {
            Ok(resp) => match resp.json::<Vec<sgp4::Elements>>().await {
                Ok(elements) => {
                    info!(
                        "TLE: fetched {} elements for group {}",
                        elements.len(),
                        group
                    );
                    all.extend(elements);
                }
                Err(e) => {
                    failed += 1;
                    error!("TLE: failed to parse group {}: {}", group, e);
                }
            },
            Err(e) => {
                failed += 1;
                error!("TLE: failed to fetch group {}: {}", group, e);
            }
        }
    }
    if failed > 0 {
        warn!(
            "TLE: {} of {} groups failed; keeping the previous elements rather \
             than caching a partial set",
            failed,
            CELESTRAK_GROUPS.len()
        );
        return None;
    }
    Some(all)
}

pub fn start_tle_refresh(cache: TleCacheHandle) {
    crate::supervised_task::spawn_supervised("tle_refresh", move || {
        let cache = cache.clone();
        async move {
            // Without an explicit timeout a blackholed network (packets
            // dropped rather than refused) leaves the request hanging
            // indefinitely, which stalls the whole refresh loop silently:
            // no elements, no error logged, and no retry.
            let client = reqwest::Client::builder()
                .timeout(TLE_REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            let mut backoff = TLE_RETRY_BASE;
            loop {
                let wait = match fetch_elements(&client).await {
                    Some(elements) if !elements.is_empty() => {
                        backoff = TLE_RETRY_BASE;
                        let n = elements.len();
                        cache.store(elements);
                        info!("TLE cache updated: {} satellites total", n);
                        TLE_REFRESH_INTERVAL
                    }
                    _ => {
                        let wait = backoff;
                        backoff = (backoff * 2).min(TLE_RETRY_MAX);
                        warn!(
                            "TLE: refresh fetched nothing usable; retrying in {:?}",
                            wait
                        );
                        wait
                    }
                };
                // Say plainly what the observer is about to be shown, so a
                // refresh that keeps failing is visible in the log well
                // before anyone notices the picker.
                match cache.status() {
                    TleStatus::Unavailable => {
                        warn!("TLE: no elements cached; the satellite picker will be empty")
                    }
                    TleStatus::Stale { age_days } => warn!(
                        "TLE: newest element is {} days old (stale past {})",
                        age_days, TLE_STALE_DAYS
                    ),
                    TleStatus::Fresh { .. } => {}
                }
                tokio::time::sleep(wait).await;
            }
        }
    });
}
