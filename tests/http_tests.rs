use chrono::{DurationRound, TimeDelta, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{COOKIE, SET_COOKIE};

mod binary_wrappers;
pub use binary_wrappers::*;

#[test]
fn can_start_and_stop_backend() {
    SalsaTestServer::spawn();
}

#[test]
fn login_with_unknown_local_user_fails() {
    let server = SalsaTestServer::spawn();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Should be possible to create reqwest client");

    let res = client
        .post(server.addr() + "/auth/local")
        .form(&[("username", "test"), ("password", "password")])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
    let location = res.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("error="),
        "Expected error redirect, got: {location}"
    );
}

#[test]
fn login_with_local_user_possible() {
    let server = SalsaTestServer::spawn();
    let user = server.add_local_user("test", "password");
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Should be possible to create reqwest client");

    let res = client
        .post(server.addr() + "/auth/local")
        .form(&[("username", &user.username), ("password", &user.password)])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
    assert!(res.headers().contains_key(SET_COOKIE));
}

#[test]
fn login_with_wrong_password_fails() {
    let server = SalsaTestServer::spawn();
    let user = server.add_local_user("test", "password");
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Should be possible to create reqwest client");

    let res = client
        .post(server.addr() + "/auth/local")
        .form(&[
            ("username", user.username.as_str()),
            ("password", "wrong_password"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
    let location = res.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("error="),
        "Expected error redirect, got: {location}"
    );
}

#[test]
fn create_booking_not_logged_in_isnt_allowed() {
    let server = SalsaTestServer::spawn();

    let client = Client::builder().cookie_store(true).build().unwrap();
    let res = client
        .post(server.addr() + "/bookings")
        .form(&[("start_timestamp", "1751331600"), ("telescope", "fake1")])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn create_booking() {
    let server = SalsaTestServer::spawn();
    let user = server.add_local_user("user", "password");

    let client = Client::builder().cookie_store(true).build().unwrap();
    server.login(&client, &user);
    let next_hour = Utc::now()
        .duration_round_up(TimeDelta::hours(1))
        .expect("Should be possible to round up to closest hour")
        .timestamp();
    let res = client
        .post(server.addr() + "/bookings")
        .form(&[
            ("start_timestamp", format!("{}", next_hour).as_str()),
            ("telescope", "fake1"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::OK, res.status());
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
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("mode=FreqSwitched")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

// TODO: Test for websocket upgrade without active booking. Requires better db
// support in these tests.

#[test]
fn interferometry_list_redirects_to_login_if_not_logged_in() {
    let server = SalsaTestServer::spawn();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let res = client
        .get(server.addr() + "/interferometry")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
    let location = res.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/auth/login"),
        "Expected login redirect, got: {location}"
    );
}

#[test]
fn cant_start_interferometry_if_not_logged_in() {
    let server = SalsaTestServer::spawn();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let res = client
        .post(server.addr() + "/interferometry/start")
        .form(&[
            ("telescope_a", "fake1"),
            ("telescope_b", "fake2"),
            ("coordinate_system", "sun"),
            ("target_x", "0"),
            ("target_y", "0"),
            ("center_freq_mhz", "1420.4"),
            ("bandwidth_mhz", "2.5"),
            ("spectral_channels", "128"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
}

#[test]
fn cant_stop_interferometry_if_not_logged_in() {
    let server = SalsaTestServer::spawn();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let res = client
        .post(server.addr() + "/interferometry/stop")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::SEE_OTHER, res.status());
}

#[test]
fn cant_fetch_interferometry_session_data_if_not_logged_in() {
    let server = SalsaTestServer::spawn();
    let client = Client::new();
    let res = client
        .get(server.addr() + "/interferometry/1/data")
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
}

#[test]
fn cant_start_interferometry_without_active_booking() {
    let server = SalsaTestServer::spawn();
    let user = server.add_local_user("interf_user", "password");
    let client = Client::builder().cookie_store(true).build().unwrap();
    server.login(&client, &user);

    let res = client
        .post(server.addr() + "/interferometry/start")
        .form(&[
            ("telescope_a", "fake1"),
            ("telescope_b", "fake2"),
            ("coordinate_system", "sun"),
            ("target_x", "0"),
            ("target_y", "0"),
            ("center_freq_mhz", "1420.4"),
            ("bandwidth_mhz", "2.5"),
            ("spectral_channels", "128"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::FORBIDDEN, res.status());
}

#[test]
fn cant_start_interferometry_with_same_telescope_twice() {
    let server = SalsaTestServer::spawn();
    let user = server.add_local_user("interf_user", "password");
    let client = Client::builder().cookie_store(true).build().unwrap();
    server.login(&client, &user);

    let res = client
        .post(server.addr() + "/interferometry/start")
        .form(&[
            ("telescope_a", "fake1"),
            ("telescope_b", "fake1"),
            ("coordinate_system", "sun"),
            ("target_x", "0"),
            ("target_y", "0"),
            ("center_freq_mhz", "1420.4"),
            ("bandwidth_mhz", "2.5"),
            ("spectral_channels", "128"),
        ])
        .send()
        .expect("Should be able to send request");

    assert_eq!(StatusCode::BAD_REQUEST, res.status());
}
