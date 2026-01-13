use reqwest::StatusCode;
use reqwest::blocking::{Client, get};
use reqwest::header::{COOKIE, SET_COOKIE};
use std::fs::copy;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use tempdir::TempDir;

struct SalsaTestServer {
    process: Child,
    port: u16,
    _database_dir: TempDir,
}

impl SalsaTestServer {
    fn spawn() -> Self {
        let database_dir =
            TempDir::new("database_dir").expect("Need to be able to create tempdir in test");
        copy(
            "telescopes.toml",
            database_dir.path().join("telescopes.toml"),
        )
        .expect("Need to be able to copy telescopes.toml in test");
        let backend_executable = env!("CARGO_BIN_EXE_backend");
        let mut process = Command::new(backend_executable)
            .args([
                "-p",
                "0",
                "--database-dir",
                database_dir
                    .path()
                    .to_str()
                    .expect("TempDir path should convert to str"),
                "--config-dir",
                "tests/test_config",
            ]) // Let the OS decide the port
            .stdout(Stdio::piped())
            .spawn()
            .expect("Could not start backend");
        let mut stdout_reader =
            BufReader::new(process.stdout.take().expect("Should be able to get stdout"));
        let mut buf = String::new();
        let res = stdout_reader.read_line(&mut buf);
        match res {
            Ok(_) => {}
            Err(err) => {
                panic!("{err}");
            }
        }
        let port = buf
            .split_once(":")
            .expect("Backend should print something with \":\"")
            .1
            .trim()
            .parse::<u16>()
            .expect("Backend should print a number");
        while let Err(_) = get(format!("http://127.0.0.1:{port}/")) {
            thread::sleep(Duration::from_millis(1));
            print!(".")
        }

        // Let the thread detach.
        thread::spawn(|| {
            for line in stdout_reader.lines() {
                match line {
                    Ok(line) => print!("{line}"),
                    Err(_) => return,
                }
            }
        });

        SalsaTestServer {
            process,
            port,
            _database_dir: database_dir,
        }
    }

    fn addr(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for SalsaTestServer {
    fn drop(&mut self) {
        self.process.kill().expect("Should be able to kill backend");
        self.process
            .wait()
            .expect("Backend should stop on kill signal");
    }
}

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
