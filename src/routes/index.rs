use askama::Template;
use axum::{
    Extension,
    extract::Query,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::i18n::Language;
use crate::models::user::User;

/// One entry in the header language picker.
struct LanguageOption {
    code: &'static str,
    /// Compact label shown in the header ("EN", "SV").
    label: String,
    /// The language's name in itself, as tooltip.
    native_name: &'static str,
    current: bool,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    name: String,
    is_admin: bool,
    is_guest: bool,
    content: String,
    build_url: String,
    version_description: String,
    /// True for a logged-in, non-guest user who hasn't set a timezone yet,
    /// triggering a one-shot browser-timezone auto-detect in the layout.
    detect_timezone: bool,
    lang: Language,
    languages: Vec<LanguageOption>,
}

#[derive(Deserialize)]
pub struct IndexQuery {
    /// Set by failed POST /observe/guest/start* redirects so the welcome
    /// page can render an explanatory banner above the hero. See
    /// `guest_error_banner` for the codes recognised.
    #[serde(default)]
    pub guest_error: Option<String>,
    /// Set when a guest session has just ended (user clicked End,
    /// idle/ceiling timeout, or preemption). Tells the visitor what
    /// happened rather than dumping them silently on the welcome page.
    #[serde(default)]
    pub guest_ended: Option<String>,
}

pub async fn get_index(
    Extension(user): Extension<Option<User>>,
    Extension(lang): Extension<Language>,
    Query(query): Query<IndexQuery>,
) -> Response {
    // guest_error takes priority over guest_ended if both happen to be set.
    let banner = query
        .guest_error
        .as_deref()
        .and_then(guest_error_banner)
        .or_else(|| query.guest_ended.as_deref().and_then(guest_ended_banner));
    // Session-ended banners (heading.is_some()) replace the hero entirely
    // — the banner's own CTAs do the job of the "Observe now"/"Book a
    // telescope" buttons. Start-failure banners sit above the hero.
    let show_hero = banner.as_ref().is_none_or(|b| b.heading.is_none());
    let content = WelcomeTemplate {
        lang,
        banner,
        show_hero,
    }
    .render()
    .expect("welcome");
    Html(render_main(user, lang, content)).into_response()
}

#[derive(Template)]
#[template(path = "welcome.html")]
struct WelcomeTemplate {
    lang: Language,
    banner: Option<WelcomeBanner>,
    show_hero: bool,
}

/// Banner texts are Fluent message keys, translated at render time.
struct WelcomeBanner {
    kind: &'static str,
    /// Some => "card" layout (heading + body + CTAs) that replaces the
    /// hero. None => plain colored callout above the hero (used by the
    /// start-failure banners).
    heading: Option<&'static str>,
    message: &'static str,
    /// Label for an "Observe again"-style POST CTA on the session-ended
    /// card. Suppressed for `ceiling` (the user just hit the 30 min cap
    /// — pointing them back at another guest session is misleading).
    observe_again_label: Option<&'static str>,
}

/// Resolve a known guest-start failure code into a banner. Unknown
/// codes return None so unexpected values from a hand-edited URL just
/// show the normal welcome page.
fn guest_error_banner(code: &str) -> Option<WelcomeBanner> {
    let (kind, message): (&'static str, &'static str) = match code {
        "all_busy" => ("warning", "guest-error-all-busy"),
        "all_maintenance" => ("warning", "guest-error-all-maintenance"),
        "busy" => ("warning", "guest-error-busy"),
        "maintenance" => ("warning", "guest-error-maintenance"),
        "guest_active" => ("warning", "guest-error-guest-active"),
        "rate_limited" => ("warning", "guest-error-rate-limited"),
        "not_found" => ("danger", "guest-error-not-found"),
        "internal" => ("danger", "guest-error-internal"),
        _ => return None,
    };
    Some(WelcomeBanner {
        kind,
        heading: None,
        message,
        observe_again_label: None,
    })
}

/// Resolve a guest-session-ended reason into a banner. Reasons match
/// the `EndReason` variants (`user`, `idle`, `ceiling`, `preempted`);
/// unknown codes return None so a hand-edited URL falls through to
/// the normal welcome page.
///
/// Tone split: `user` and `ceiling` are normal session conclusions
/// (info blue); `idle` and `preempted` are involuntary ends (warning
/// yellow).
fn guest_ended_banner(reason: &str) -> Option<WelcomeBanner> {
    let (kind, heading, message, observe_again): (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    ) = match reason {
        "user" => (
            "info",
            "guest-ended-user-heading",
            "guest-ended-user-message",
            Some("welcome-observe-again"),
        ),
        "idle" => (
            "warning",
            "guest-ended-idle-heading",
            "guest-ended-idle-message",
            Some("welcome-observe-again"),
        ),
        "ceiling" => (
            "info",
            "guest-ended-ceiling-heading",
            "guest-ended-ceiling-message",
            None,
        ),
        "preempted" => (
            "warning",
            "guest-ended-preempted-heading",
            "guest-ended-preempted-message",
            Some("welcome-try-another"),
        ),
        _ => return None,
    };
    Some(WelcomeBanner {
        kind,
        heading: Some(heading),
        message,
        observe_again_label: observe_again,
    })
}

const GITHUB_SERVER_URL: Option<&'static str> = option_env!("GITHUB_SERVER_URL");
const GITHUB_REPOSITORY: Option<&'static str> = option_env!("GITHUB_REPOSITORY");

pub fn render_main(user: Option<User>, lang: Language, content: String) -> String {
    let build_url = match (GITHUB_SERVER_URL, GITHUB_REPOSITORY) {
        (Some(server_url), Some(repository)) => format!(
            "{}/{}/releases/tag/v{}",
            server_url,
            repository,
            env!("CARGO_PKG_VERSION")
        ),
        _ => String::new(),
    };
    let version_description = if build_url.is_empty() {
        format!(
            "v{}, on branch {}",
            env!("CARGO_PKG_VERSION"),
            env!("GIT_BRANCH_NAME")
        )
    } else {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    };
    let is_admin = user.as_ref().is_some_and(|u| u.is_admin);
    let is_guest = user.as_ref().is_some_and(|u| u.provider == "guest");
    let detect_timezone = user
        .as_ref()
        .is_some_and(|u| u.provider != "guest" && u.timezone.is_none());
    let name = match &user {
        Some(u) => u.name.clone(),
        None => String::new(),
    };
    let languages = Language::ALL
        .iter()
        .map(|&language| LanguageOption {
            code: language.code(),
            label: language.code().to_ascii_uppercase(),
            native_name: language.native_name(),
            current: language == lang,
        })
        .collect();
    IndexTemplate {
        name,
        is_admin,
        is_guest,
        content,
        build_url,
        version_description,
        detect_timezone,
        lang,
        languages,
    }
    .render()
    .expect("Template should always succeed")
}
