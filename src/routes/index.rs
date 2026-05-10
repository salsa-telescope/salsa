use std::fs::read_to_string;

use askama::Template;
use axum::{
    Extension,
    response::{Html, IntoResponse, Response},
};

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

pub async fn get_index(Extension(user): Extension<Option<User>>) -> Response {
    Html(render_main(
        user,
        // TODO: Read this file at startup.
        read_to_string("assets/welcome.html").expect("Reading static data should always work"),
    ))
    .into_response()
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
