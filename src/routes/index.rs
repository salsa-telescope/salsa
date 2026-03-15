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
const GITHUB_RUN_ID: Option<&'static str> = option_env!("GITHUB_RUN_ID");

pub fn render_main(user: Option<User>, content: String) -> String {
    let build_url = match (GITHUB_SERVER_URL, GITHUB_REPOSITORY, GITHUB_RUN_ID) {
        (Some(server_url), Some(repository), Some(run_id)) => {
            format!("{}/{}/actions/runs/{}", server_url, repository, run_id)
        }
        _ => String::new(),
    };
    let is_admin = user.as_ref().is_some_and(|u| u.is_admin);
    let name = match &user {
        Some(u) => u.name.clone(),
        None => String::new(),
    };
    IndexTemplate {
        name,
        is_admin,
        content,
        build_url,
        version_description: format!("local build on branch {}", env!("GIT_BRANCH_NAME")),
    }
    .render()
    .expect("Template should always succeed")
}
