use std::collections::HashMap;

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use axum::{extract::Request, middleware::Next, response::Response};

/// Cookies sent by the client, keyed by name. A name maps to multiple values
/// when the client holds several cookies with the same name (e.g. one set by
/// us and one set with a Domain attribute on a parent domain by some other
/// service) — consumers decide which value, if any, is meaningful.
#[derive(Clone)]
pub struct Cookies(pub HashMap<String, Vec<String>>);

impl Cookies {
    pub fn get_all(&self, name: &str) -> &[String] {
        self.0.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub async fn cookies_middleware(headers: HeaderMap, mut request: Request, next: Next) -> Response {
    let mut cookies = HashMap::new();
    // HTTP/2 clients are allowed to split cookies over multiple Cookie
    // headers (RFC 9113 section 8.2.3), so read them all.
    for header_value in headers.get_all(COOKIE) {
        if let Ok(cookies_header) = header_value.to_str() {
            parse_cookies_into(cookies_header, &mut cookies);
        }
    }
    request.extensions_mut().insert(Cookies(cookies));
    next.run(request).await
}

/// Parse a cookie header per RFC 6265 section 5.4, leniently: fragments that
/// don't look like name=value are skipped rather than failing the request,
/// since any service on a parent domain can plant a malformed cookie that we
/// then receive on every request.
fn parse_cookies_into(cookie_header: &str, cookies: &mut HashMap<String, Vec<String>>) {
    for fragment in cookie_header.split(';') {
        let fragment = fragment.trim();
        let Some((name, value)) = fragment.split_once('=') else {
            continue;
        };
        if name.is_empty() {
            // Nameless cookies are ambiguous; ignore them.
            continue;
        }
        cookies
            .entry(name.to_string())
            .or_default()
            .push(value.to_string());
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn parse(header: &str) -> HashMap<String, Vec<String>> {
        let mut cookies = HashMap::new();
        parse_cookies_into(header, &mut cookies);
        cookies
    }

    #[test]
    fn empty() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn one_cookie() {
        let map = parse("foo=bar");
        assert_eq!(1, map.len());
        assert_eq!(vec!["bar"], map["foo"]);
    }

    #[test]
    fn two_cookies() {
        let map = parse("foo=bar; peti=kloe");
        assert_eq!(2, map.len());
        assert_eq!(vec!["bar"], map["foo"]);
        assert_eq!(vec!["kloe"], map["peti"]);
    }

    #[test]
    fn duplicate_names_keep_all_values_in_order() {
        let map = parse("foo=first; foo=second");
        assert_eq!(1, map.len());
        assert_eq!(vec!["first", "second"], map["foo"]);
    }

    #[test]
    fn malformed_fragment_is_skipped() {
        let map = parse("foo:bar; peti=kloe");
        assert_eq!(1, map.len());
        assert_eq!(vec!["kloe"], map["peti"]);
    }

    #[test]
    fn nameless_cookie_is_skipped() {
        let map = parse("=bar; peti=kloe");
        assert_eq!(1, map.len());
        assert_eq!(vec!["kloe"], map["peti"]);
    }

    #[test]
    fn missing_space_after_semicolon_still_parses() {
        let map = parse("foo=bar;peti=kloe");
        assert_eq!(2, map.len());
        assert_eq!(vec!["bar"], map["foo"]);
        assert_eq!(vec!["kloe"], map["peti"]);
    }

    #[test]
    fn value_may_contain_equals_sign() {
        let map = parse("token=abc=def==");
        assert_eq!(vec!["abc=def=="], map["token"]);
    }
}
