use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{COOKIE, SET_COOKIE};

mod binary_wrappers;
use binary_wrappers::SalsaTestServer;

#[test]
fn can_start_and_stop_backend() {
    SalsaTestServer::spawn();
}

#[test]
fn create_booking_not_logged_in_isnt_allowed() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .post(server.addr() + "/bookings")
        .form(&[
            ("start_date", "2025-07-01"),
            ("start_time", "02:00:00"),
            ("telescope", "fake1"),
            ("duration", "1"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn invalid_cookie_header_gives_bad_request() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .get(server.addr())
        .header(COOKIE, "a_cookie:thisisnthowitsdone")
        .send()
        .expect("Requst should complete");

    assert_eq!(StatusCode::BAD_REQUEST, res.status())
}

#[test]
fn invalid_session_is_200_ok_and_resets_cookie() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .get(server.addr())
        .header(COOKIE, "session=notavaildsession")
        .send()
        .expect("Request should complete");

    assert_eq!(StatusCode::OK, res.status());
    assert_eq!(
        "session=deleted; expires=Thu, 01 Jan 1970 00:00:00 GMT",
        res.headers()[SET_COOKIE]
    );
}

#[test]
fn cant_open_websocket_for_spectrum_if_not_logged_in() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .get(server.addr() + "/telescope/fake1/spectrum")
        .header("Connection", "upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Key", "test")
        .header("Sec-WebSocket-Version", "13")
        .send()
        .expect("Request should complete");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn cant_observe_if_not_logged_in() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .get(server.addr() + "/observe/fake1")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn cant_set_target_if_not_logged_in() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .post(server.addr() + "/observe/fake1/set-target")
        .form(&[("x", "42"), ("y", "90"), ("coordinate_system", "galactic")])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn cant_start_observation_if_not_logged_in() {
    let server = SalsaTestServer::spawn();

    let client = Client::new();
    let res = client
        .post(server.addr() + "/observe/fake1/observe")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

// TODO: Test for websocket upgrade without active booking. Requires better db
// support in these tests.
