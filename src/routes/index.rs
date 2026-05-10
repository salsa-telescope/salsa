use std::fs::read_to_string;

use askama::Template;
use axum::{
    Extension,
    extract::Query,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::models::user::User;

#[derive(Template)]
#[template(path = "index.html", escape = "none")]
struct IndexTemplate {
    name: String,
    is_admin: bool,
    is_guest: bool,
    content: String,
    build_url: String,
    version_description: String,
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
    // TODO: Read this file at startup.
    let mut content =
        read_to_string("assets/welcome.html").expect("Reading static data should always work");
    if let Some(banner) = query.guest_error.as_deref().and_then(guest_error_banner) {
        content = format!("{}{}", banner.render().expect("welcome_banner"), content);
    } else if let Some(banner) = query.guest_ended.as_deref().and_then(guest_ended_banner) {
        content = format!("{}{}", banner.render().expect("welcome_banner"), content);
    }
    Html(render_main(user, content)).into_response()
}

#[derive(Template)]
#[template(path = "welcome_banner.html")]
struct WelcomeBanner {
    kind: &'static str,
    message: &'static str,
}

/// Resolve a known guest-start failure code into a banner. Unknown
/// codes return None so unexpected values from a hand-edited URL just
/// show the normal welcome page.
fn guest_error_banner(code: &str) -> Option<WelcomeBanner> {
    let (kind, message) = match code {
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
    Some(WelcomeBanner { kind, message })
}

/// Resolve a guest-session-ended reason into a banner. Reasons match
/// the `EndReason` variants (`user`, `idle`, `ceiling`, `preempted`);
/// unknown codes return None so a hand-edited URL falls through to
/// the normal welcome page.
fn guest_ended_banner(reason: &str) -> Option<WelcomeBanner> {
    let message = match reason {
        "user" => {
            "Your guest session has ended. Thanks for trying SALSA — \
             click <strong>Observe now</strong> to start another, or \
             create a free account to reserve a longer time slot."
        }
        "idle" => {
            "Your guest session ended due to inactivity. \
             Click <strong>Observe now</strong> to start another, or \
             create a free account to reserve a longer time slot."
        }
        "ceiling" => {
            "Your guest session reached the 30-minute maximum and ended. \
             Create a free account to reserve a longer time slot."
        }
        "preempted" => {
            "Your guest session ended because a registered user booked this telescope. \
             Click <strong>Observe now</strong> to try another telescope, or \
             create a free account to reserve your own time slot."
        }
        _ => return None,
    };
    Some(WelcomeBanner {
        kind: "info",
        message,
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
    }
    .render()
    .expect("Template should always succeed")
}
