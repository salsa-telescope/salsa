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
}

pub async fn get_index(
    Extension(user): Extension<Option<User>>,
    Query(query): Query<IndexQuery>,
) -> Response {
    // TODO: Read this file at startup.
    let mut content =
        read_to_string("assets/welcome.html").expect("Reading static data should always work");
    if let Some(banner) = query.guest_error.as_deref().and_then(guest_error_banner) {
        content = format!("{banner}{content}");
    }
    Html(render_main(user, content)).into_response()
}

/// Render a styled welcome-page banner for a known guest-start failure
/// code. Unknown codes return None so unexpected values from a
/// hand-edited URL just show the normal welcome page.
fn guest_error_banner(code: &str) -> Option<String> {
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
    let (bg, border, text) = match kind {
        "danger" => ("bg-red-50", "border-red-300", "text-red-700"),
        _ => ("bg-warning-light", "border-warning-border", "text-warning"),
    };
    Some(format!(
        "<div class=\"section light\">\
           <div class=\"max-w-3xl mx-auto text-sm font-semibold {text} {bg} border {border} rounded px-4 py-3\">\
             {message}\
           </div>\
         </div>"
    ))
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
