use std::sync::LazyLock;

use i18n_embed::{
    LanguageLoader,
    fluent::{FluentLanguageLoader, fluent_language_loader},
};
use rust_embed::RustEmbed;
use unic_langid::LanguageIdentifier;

/// Fluent catalogs under i18n/<code>/salsa.ftl, embedded into the binary
/// at compile time. Editing a catalog requires a rebuild, like templates.
#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

/// Languages the UI is available in. Adding a language means adding a
/// variant here (plus its entries in the methods below) and a catalog
/// under i18n/<code>/ — everything else picks it up from [`Language::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    English,
    Swedish,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::English, Language::Swedish];

    /// ISO 639-1 code, used in the cookie, the database and `<html lang>`.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Swedish => "sv",
        }
    }

    /// The language's name in itself, for the picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Swedish => "Svenska",
        }
    }

    /// Parse a bare code ("sv") or a code with region ("sv-SE").
    pub fn from_code(code: &str) -> Option<Language> {
        let primary = code.split(['-', '_']).next().unwrap_or(code);
        match primary.to_ascii_lowercase().as_str() {
            "en" => Some(Language::English),
            "sv" => Some(Language::Swedish),
            _ => None,
        }
    }

    /// First supported language in an Accept-Language header. Entries are
    /// comma-separated with optional ";q=" weights; browsers already order
    /// them by preference, so the first match is the user's best choice
    /// and we skip q-value arithmetic.
    pub fn from_accept_language(header: &str) -> Option<Language> {
        header
            .split(',')
            .filter_map(|entry| entry.split(';').next())
            .find_map(|code| Language::from_code(code.trim()))
    }

    pub fn loader(self) -> &'static FluentLanguageLoader {
        match self {
            Language::English => &EN_LOADER,
            Language::Swedish => &SV_LOADER,
        }
    }

    /// Look up a message by key. Missing keys fall back to English (the
    /// loader always includes the fallback catalog), so a not-yet-
    /// translated string renders in English rather than breaking the page.
    /// Rust code with literal keys should prefer `i18n_embed_fl::fl!`,
    /// which checks the key against the English catalog at compile time.
    pub fn t(self, key: &str) -> String {
        self.loader().get(key)
    }
}

fn load(code: &str) -> FluentLanguageLoader {
    let loader = fluent_language_loader!();
    let id: LanguageIdentifier = code.parse().expect("Hardcoded language code is valid");
    loader
        .load_languages(&Localizations, &[id])
        .expect("Embedded catalog for a supported language should load");
    loader
}

static EN_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| load("en"));
static SV_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| load("sv"));

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn from_code_matches_primary_subtag() {
        assert_eq!(Some(Language::Swedish), Language::from_code("sv"));
        assert_eq!(Some(Language::Swedish), Language::from_code("sv-SE"));
        assert_eq!(Some(Language::English), Language::from_code("en_US"));
        assert_eq!(None, Language::from_code("fi"));
    }

    #[test]
    fn accept_language_picks_first_supported() {
        assert_eq!(
            Some(Language::Swedish),
            Language::from_accept_language("sv-SE,sv;q=0.9,en;q=0.8")
        );
        assert_eq!(
            Some(Language::English),
            Language::from_accept_language("de-DE,en;q=0.7,sv;q=0.5")
        );
        assert_eq!(None, Language::from_accept_language("de-DE,fi;q=0.8"));
    }

    #[test]
    fn lookup_translates_and_falls_back() {
        assert_eq!("Bookings", Language::English.t("nav-bookings"));
        assert_eq!("Bokningar", Language::Swedish.t("nav-bookings"));
    }

    /// Message keys are lines starting (unindented) with `key =`; indented
    /// lines are multiline-value continuations and `#` lines are comments.
    fn ftl_keys(source: &str) -> std::collections::BTreeSet<&str> {
        source
            .lines()
            .filter(|line| !line.starts_with([' ', '\t', '#']))
            .filter_map(|line| {
                let (key, _) = line.split_once('=')?;
                let key = key.trim();
                let valid = !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                valid.then_some(key)
            })
            .collect()
    }

    /// Guards against catalog drift: every key must exist in every
    /// language. A missing Swedish key would silently render in English
    /// (by design), so only a test makes the gap visible.
    #[test]
    fn catalogs_have_identical_key_sets() {
        let en = ftl_keys(include_str!("../i18n/en/salsa.ftl"));
        let sv = ftl_keys(include_str!("../i18n/sv/salsa.ftl"));
        assert!(!en.is_empty(), "English catalog parsed to zero keys");
        let missing_in_sv: Vec<_> = en.difference(&sv).collect();
        let missing_in_en: Vec<_> = sv.difference(&en).collect();
        assert!(
            missing_in_sv.is_empty() && missing_in_en.is_empty(),
            "Catalog key sets differ. Missing in sv: {missing_in_sv:?}. Missing in en: {missing_in_en:?}"
        );
    }
}
