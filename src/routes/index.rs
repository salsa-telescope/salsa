use askama::Template;
use axum::{
    Extension,
    extract::Query,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::models::user::User;

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
    let content = WelcomeTemplate { banner, show_hero }
        .render()
        .expect("welcome");
    Html(render_main(user, content)).into_response()
}

#[derive(Template)]
#[template(path = "welcome.html")]
struct WelcomeTemplate {
    banner: Option<WelcomeBanner>,
    show_hero: bool,
}

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
        "all_busy" => (
            "warning",
            "All telescopes are currently in use. Please try again in a few minutes, \
             or create a free account to reserve a time slot.",
        ),
        "all_maintenance" => (
            "warning",
            "All telescopes are currently in maintenance. Please try again later.",
        ),
        "busy" => (
            "warning",
            "That telescope is currently booked. Please try again later, \
             or create a free account to reserve a time slot.",
        ),
        "maintenance" => (
            "warning",
            "That telescope is currently in maintenance. Please try again later.",
        ),
        "guest_active" => (
            "warning",
            "Another guest is currently using that telescope. Please try again in a moment.",
        ),
        "rate_limited" => (
            "warning",
            "Too many guest sessions started from your address. Please wait a few minutes, \
             or create a free account to reserve a time slot.",
        ),
        "not_found" => ("danger", "Telescope not found."),
        "internal" => (
            "danger",
            "Something went wrong starting the guest session. Please try again.",
        ),
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
            "Session ended",
            "Thanks for trying SALSA.",
            Some("Observe again"),
        ),
        "idle" => (
            "warning",
            "Session timed out",
            "Your guest session ended due to inactivity.",
            Some("Observe again"),
        ),
        "ceiling" => (
            "info",
            "30-minute limit reached",
            "Your guest session reached the maximum length for unregistered visitors.",
            None,
        ),
        "preempted" => (
            "warning",
            "Telescope reserved by another user",
            "Your guest session ended because a registered user booked this telescope.",
            Some("Try another telescope"),
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

pub fn render_main(user: Option<User>, content: String) -> String {
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
    IndexTemplate {
        name,
        is_admin,
        is_guest,
        content,
        build_url,
        version_description,
        detect_timezone,
    }
    .render()
    .expect("Template should always succeed")
}
