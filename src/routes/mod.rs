use crate::i18n::Language;

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
