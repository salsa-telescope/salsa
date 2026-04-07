use chrono::Utc;
use std::sync::{Arc, RwLock};
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

const WEATHER_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const WEATHER_URL: &str = "https://www.oso.chalmers.se/weather/onsala.txt";

#[derive(Clone, Debug)]
pub struct WeatherData {
    pub timestamp: i64,
    pub temp_c: f64,
    pub pressure_hpa: f64,
    pub humidity_pct: f64,
    /// 10-minute average wind speed in m/s
    pub wind_avg_ms: f64,
    /// Wind direction in degrees (0 = N, 90 = E, …)
    pub wind_dir_deg: f64,
    /// 3-second gust maximum over last 10 minutes in m/s
    pub wind_gust_ms: f64,
    /// 3-second lull minimum over last 10 minutes in m/s
    pub wind_lull_ms: f64,
}

impl WeatherData {
    pub fn wind_compass(&self) -> &'static str {
        deg_to_compass(self.wind_dir_deg)
    }

    /// Age of the measurement in seconds relative to now.
    pub fn age_secs(&self) -> i64 {
        (Utc::now().timestamp() - self.timestamp).max(0)
    }
}

fn deg_to_compass(deg: f64) -> &'static str {
    let idx = ((deg + 11.25) / 22.5) as usize % 16;
    [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ][idx]
}

#[derive(Clone)]
pub struct WeatherCacheHandle {
    data: Arc<RwLock<Option<WeatherData>>>,
}

impl Default for WeatherCacheHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl WeatherCacheHandle {
    pub fn new() -> Self {
        WeatherCacheHandle {
            data: Arc::new(RwLock::new(None)),
        }
    }

    pub fn get(&self) -> Option<WeatherData> {
        self.data.read().unwrap().clone()
    }
}

async fn fetch_weather(client: &reqwest::Client) -> Option<WeatherData> {
    let text = match client.get(WEATHER_URL).send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                error!("Weather: failed to read response body: {e}");
                return None;
            }
        },
        Err(e) => {
            error!("Weather: failed to fetch {WEATHER_URL}: {e}");
            return None;
        }
    };

    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 8 {
        warn!("Weather: unexpected data format: {text:?}");
        return None;
    }

    let parsed = (|| -> Option<WeatherData> {
        Some(WeatherData {
            timestamp: parts[0].parse().ok()?,
            temp_c: parts[1].parse().ok()?,
            pressure_hpa: parts[2].parse().ok()?,
            humidity_pct: parts[3].parse().ok()?,
            wind_avg_ms: parts[4].parse().ok()?,
            wind_dir_deg: parts[5].parse().ok()?,
            wind_gust_ms: parts[6].parse().ok()?,
            wind_lull_ms: parts[7].parse().ok()?,
        })
    })();

    if parsed.is_none() {
        warn!("Weather: failed to parse values from: {text:?}");
    }
    parsed
}

pub fn start_weather_refresh(cache: WeatherCacheHandle) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Building reqwest client should not fail");
        // First tick fires immediately, then every 5 minutes
        let mut ticker = interval(WEATHER_REFRESH_INTERVAL);
        loop {
            ticker.tick().await;
            if let Some(data) = fetch_weather(&client).await {
                info!(
                    "Weather cache updated: {:.1}°C, {:.1} m/s {}",
                    data.temp_c,
                    data.wind_avg_ms,
                    data.wind_compass()
                );
                *cache.data.write().unwrap() = Some(data);
            }
        }
    });
}
