use crate::i18n::Language;
use serde::{Deserialize, Deserializer};

/// Deserialize an optional number from a form or query field that the user
/// may have left blank.
///
/// A blank `<input type="number">` is still submitted, as `field=`, and plain
/// `Option<f64>` rejects that: serde tries to parse the empty string as a
/// number, the whole form fails to deserialize, and the request is answered
/// with a bare 422 before any handler code runs. An empty field means "not
/// given" here, so map it to `None`.
///
/// Use with `#[serde(default, deserialize_with = "empty_as_none")]` — the
/// `default` covers the field being absent altogether, which happens when the
/// input is disabled.
pub fn empty_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Read a static content page from `assets/`, preferring a translated
/// variant. For Swedish, `assets/<name>.sv.html` is tried first and the
/// English `assets/<name>.html` is the fallback, so content pages can be
/// translated one at a time. `fallback` is served if neither file exists.
pub fn read_content_page(name: &str, lang: Language, fallback: &str) -> String {
    let localized = match lang {
        Language::English => None,
        _ => std::fs::read_to_string(format!("assets/{}.{}.html", name, lang.code())).ok(),
    };
    localized
        .or_else(|| std::fs::read_to_string(format!("assets/{name}.html")).ok())
        .unwrap_or_else(|| fallback.to_string())
}

pub mod about;
pub mod account;
pub mod admin;
pub mod authentication;
pub mod booking;
pub mod experiments;
pub mod index;
pub mod interferometry;
pub mod language;
pub mod live;
pub mod observations;
pub mod observe;
pub mod support;
pub mod technical;
pub mod telescope;
pub mod visibility;
pub mod weather;
