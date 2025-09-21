use std::collections::HashMap;

use axum::http::HeaderMap;
use axum::http::{StatusCode, header::COOKIE};
use axum::{extract::Request, middleware::Next, response::Response};

#[derive(Clone)]
pub struct Cookies(pub HashMap<String, String>);

pub async fn cookies_middleware(
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cookies = if let Some(cookies_header_value) = headers.get(COOKIE) {
        let cookies_header = cookies_header_value
            .to_str()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        parse_cookies(cookies_header).map_err(|_| StatusCode::BAD_REQUEST)?
    } else {
        HashMap::new()
    };
    request.extensions_mut().insert(Cookies(cookies));
    Ok(next.run(request).await)
}

#[derive(Debug)]
pub struct ParseError {}

/// Parse cookie headers
fn parse_cookies(cookie_header: &str) -> Result<HashMap<String, String>, ParseError> {
    // > 5.4 The Cookie Header
    // > [...]
    // > 4.  Serialize the cookie-list into a cookie-string by processing each
    // >        cookie in the cookie-list in order:
    // >
    // >        1.  Output the cookie's name, the %x3D ("=") character, and the
    // >            cookie's value.
    // >
    // >        2.  If there is an unprocessed cookie in the cookie-list, output
    // >            the characters %x3B and %x20 ("; ").
    // [https://datatracker.ietf.org/doc/html/rfc6265]
    //
    // We will reject nameless cookies. Their meaaning is ambiguous and if we
    // can avoid them that's good.
    //
    // We're liberal in what we accept as names or values for cookies, anything
    // goes that's valid utf-8. This is outside spec so can be made more strict
    // if needed.

    let mut res = HashMap::new();
    for cookie_string in cookie_header.split("; ") {
        if cookie_string.is_empty() {
            continue;
        }
        let (name, value) = cookie_string.split_once('=').ok_or(ParseError {})?;
        res.insert(name.to_string(), value.to_string());
    }
    Ok(res)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn empty() {
        let res = parse_cookies("");
        assert!(res.is_ok());
        assert!(res.unwrap().is_empty())
    }

    #[test]
    fn one_cookie() {
        let res = parse_cookies("foo=bar");
        assert!(res.is_ok());
        let map = res.unwrap();
        assert_eq!(1, map.len());
        assert_eq!("bar", map["foo"]);
    }

    #[test]
    fn two_cookies() {
        let res = parse_cookies("foo=bar; peti=kloe");
        assert!(res.is_ok());
        let map = res.unwrap();
        assert_eq!(2, map.len());
        assert_eq!("bar", map["foo"]);
        assert_eq!("kloe", map["peti"]);
    }

    #[test]
    fn wrong_format() {
        let res = parse_cookies("foo:bar");
        assert!(res.is_err());
    }

    #[test]
    fn missing_space() {
        let res = parse_cookies("foo=bar;peti=kloe");
        assert!(res.is_ok());
        let map = res.unwrap();
        assert_eq!(1, map.len());
        // Not quite sure this is the correct behavior. Change if required.
        assert_eq!("bar;peti=kloe", map["foo"]);
    }
}
