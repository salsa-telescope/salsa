use crate::app::AppState;
use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;

pub fn routes(state: AppState) -> Router {
    Router::new().route("/", get(get_weather)).with_state(state)
}

#[derive(Template)]
#[template(path = "weather.html", escape = "none")]
struct WeatherTemplate {
    age_str: String,
    weather_ts: i64,
    temp_c: String,
    pressure_hpa: String,
    humidity_pct: String,
    wind_avg_ms: String,
    compass: String,
    wind_dir_deg: String,
    wind_gust_ms: String,
    wind_lull_ms: String,
}

async fn get_weather(State(state): State<AppState>) -> Html<String> {
    let Some(w) = state.weather_cache.get() else {
        return Html(
            r#"<p class="text-xs text-gray-400 mt-2">Weather data unavailable.</p>"#.to_string(),
        );
    };

    let age_secs = w.age_secs();
    let age_str = if age_secs < 120 {
        format!("{age_secs}s ago")
    } else {
        format!("{}min ago", age_secs / 60)
    };

    let html = WeatherTemplate {
        age_str,
        weather_ts: w.timestamp,
        temp_c: format!("{:.1}", w.temp_c),
        pressure_hpa: format!("{:.0}", w.pressure_hpa),
        humidity_pct: format!("{:.0}", w.humidity_pct),
        wind_avg_ms: format!("{:.1}", w.wind_avg_ms),
        compass: w.wind_compass().to_string(),
        wind_dir_deg: format!("{:.0}", w.wind_dir_deg),
        wind_gust_ms: format!("{:.1}", w.wind_gust_ms),
        wind_lull_ms: format!("{:.1}", w.wind_lull_ms),
    }
    .render()
    .expect("Template rendering should always succeed");

    Html(html)
}
