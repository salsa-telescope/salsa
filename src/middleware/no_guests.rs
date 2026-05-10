//! Reject guest users at the route-layer level.
//!
//! Guest sessions exist solely so anonymous visitors can try the
//! observe page; they have no business reaching booking creation,
//! observation archive listings, or account management. This
//! middleware redirects any guest to the observe page (which itself
//! redirects to their active telescope) instead of letting the
//! normal handler render. Apply with `.route_layer(...)` on the
//! affected route trees.

use axum::{
    Extension,
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};

use crate::models::user::User;

pub async fn reject_guests(
    Extension(user): Extension<Option<User>>,
    request: Request,
    next: Next,
) -> Response {
    if user.as_ref().is_some_and(|u| u.provider == "guest") {
        return Redirect::to("/observe").into_response();
    }
    next.run(request).await
}
