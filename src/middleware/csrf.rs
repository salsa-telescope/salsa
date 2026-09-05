//! Reject state-changing requests that come from another origin.
//!
//! The session cookie is `SameSite=Lax`, which keeps a genuinely cross-*site*
//! page from submitting a form with the user's cookie attached. What it does
//! not cover is a same-site one: SameSite is scoped to the registrable domain,
//! so every page under `chalmers.se` counts as same-site as us, and its form
//! posts carry our session cookie. One XSS on an unrelated university host is
//! then enough to move a telescope, or to drive the admin pages, on behalf of
//! whoever visits it.
//!
//! Comparing `Origin` against the host the request was actually addressed to
//! closes that: a browser sets `Origin` on every cross-origin form post and
//! `fetch`, and it names the attacking page's own origin, which is never ours.
//!
//! Requests carrying no `Origin` at all are let through. Non-browser clients —
//! curl, the test suite, uptime checks — send none, while the attack this
//! guards against is by definition made by a browser, which does. So the check
//! is worth what it costs only for requests that have one; demanding the header
//! outright would break those clients without closing anything extra.

use axum::{
    extract::Request,
    http::{
        HeaderMap, Method, StatusCode, Uri,
        header::{HOST, ORIGIN},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::warn;

/// Methods that can change server state. Everything else is a read, and a
/// cross-origin read is already governed by the same-origin policy.
fn is_state_changing(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// The `host[:port]` part of an `Origin` header.
///
/// `None` for anything that is not a well-formed absolute origin — including
/// the literal `null` that sandboxed iframes and some privacy tools send,
/// which must not be mistaken for "no origin at all".
fn origin_authority(origin: &str) -> Option<&str> {
    let (_scheme, authority) = origin.split_once("://")?;
    (!authority.is_empty()).then_some(authority)
}

/// The `host[:port]` the request was addressed to.
///
/// HTTP/1.1 puts it in the `Host` header; HTTP/2 puts it in `:authority`,
/// which arrives as the URI's authority and leaves no `Host` header behind.
/// TLS here negotiates h2, so both paths are live in production.
fn request_authority<'a>(uri: &'a Uri, headers: &'a HeaderMap) -> Option<&'a str> {
    uri.authority()
        .map(|authority| authority.as_str())
        .or_else(|| headers.get(HOST).and_then(|value| value.to_str().ok()))
}

/// Whether this request may proceed: either it carries no `Origin`, or the one
/// it carries names the host it was sent to.
fn origin_is_trusted(uri: &Uri, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    let (Some(origin), Some(target)) = (origin_authority(origin), request_authority(uri, headers))
    else {
        return false;
    };
    origin.eq_ignore_ascii_case(target)
}

pub async fn origin_check_middleware(request: Request, next: Next) -> Response {
    if is_state_changing(request.method()) && !origin_is_trusted(request.uri(), request.headers()) {
        warn!(
            "Rejecting cross-origin {} {}: Origin {:?} does not match host {:?}",
            request.method(),
            request.uri().path(),
            request.headers().get(ORIGIN),
            request_authority(request.uri(), request.headers()),
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod test {
    use super::*;
    use axum::body::Body;
    use axum::{Router, routing::post};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/change", post(|| async { "ok" }))
            .route("/read", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(origin_check_middleware))
    }

    /// `headers` are added on top of a `Host: salsa.example` request.
    async fn request(method: Method, path: &str, headers: &[(&str, &str)]) -> StatusCode {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "salsa.example");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        app()
            .oneshot(builder.body(Body::empty()).expect("request should build"))
            .await
            .expect("router should respond")
            .status()
    }

    /// The page's own forms and HTMX calls: same origin, so they pass.
    #[tokio::test]
    async fn same_origin_post_is_allowed() {
        assert_eq!(
            request(
                Method::POST,
                "/change",
                &[("origin", "https://salsa.example")]
            )
            .await,
            StatusCode::OK
        );
    }

    /// The whole point: a sibling host under the same registrable domain is
    /// same-*site*, so SameSite=Lax lets its post through with our cookie.
    /// This is the layer that stops it.
    #[tokio::test]
    async fn sibling_site_post_is_rejected() {
        assert_eq!(
            request(
                Method::POST,
                "/change",
                &[("origin", "https://other.example")]
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    /// DELETE reaches the observation and booking archives over HTMX, so it
    /// has to be covered as well as POST.
    #[tokio::test]
    async fn cross_origin_delete_is_rejected() {
        assert_eq!(
            request(
                Method::DELETE,
                "/change",
                &[("origin", "https://other.example")]
            )
            .await,
            // The route only accepts POST; what matters is that the request
            // never got far enough to be told so.
            StatusCode::FORBIDDEN
        );
    }

    /// curl, the test suite and uptime checks send no Origin, and the attack
    /// this guards against cannot avoid sending one.
    #[tokio::test]
    async fn post_without_an_origin_is_allowed() {
        assert_eq!(request(Method::POST, "/change", &[]).await, StatusCode::OK);
    }

    /// A sandboxed iframe posts `Origin: null`. It is not this host, and it
    /// must not be read as an absent header.
    #[tokio::test]
    async fn null_origin_is_rejected() {
        assert_eq!(
            request(Method::POST, "/change", &[("origin", "null")]).await,
            StatusCode::FORBIDDEN
        );
    }

    /// Reads are governed by the same-origin policy already, and blocking
    /// them would break ordinary cross-origin navigation to the site.
    #[tokio::test]
    async fn cross_origin_get_is_allowed() {
        assert_eq!(
            request(Method::GET, "/read", &[("origin", "https://other.example")]).await,
            StatusCode::OK
        );
    }

    /// Hostnames are case-insensitive, and a browser is free to send the
    /// origin in a different case than the Host header carries.
    #[tokio::test]
    async fn host_comparison_ignores_case() {
        assert_eq!(
            request(
                Method::POST,
                "/change",
                &[("origin", "https://SALSA.example")]
            )
            .await,
            StatusCode::OK
        );
    }

    /// Dev runs on a port, production does not; the port is part of the
    /// origin and has to match either way.
    #[test]
    fn authority_comparison_includes_the_port() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "localhost:3000".parse().unwrap());
        let uri: Uri = "/change".parse().unwrap();

        headers.insert(ORIGIN, "http://localhost:3000".parse().unwrap());
        assert!(origin_is_trusted(&uri, &headers));

        headers.insert(ORIGIN, "http://localhost:3001".parse().unwrap());
        assert!(!origin_is_trusted(&uri, &headers));
    }

    /// HTTP/2 carries the host in `:authority`, which reaches us as the URI's
    /// authority with no Host header at all.
    #[test]
    fn http2_authority_is_used_when_there_is_no_host_header() {
        let uri: Uri = "https://salsa.example/change".parse().unwrap();
        let mut headers = HeaderMap::new();

        headers.insert(ORIGIN, "https://salsa.example".parse().unwrap());
        assert!(origin_is_trusted(&uri, &headers));

        headers.insert(ORIGIN, "https://other.example".parse().unwrap());
        assert!(!origin_is_trusted(&uri, &headers));
    }
}
