use reqwest::StatusCode;
use reqwest::blocking::{Client, get};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

struct SalsaTestServer {
    process: Child,
    port: u16,
}

impl SalsaTestServer {
    fn spawn() -> Self {
        let backend_executable = env!("CARGO_BIN_EXE_backend");
        let mut process = Command::new(backend_executable)
            .args(["-p", "0"]) // Let the OS decide the port
            .stdout(Stdio::piped())
            .spawn()
            .expect("Could not start backend");
        let mut stdout_reader =
            BufReader::new(process.stdout.take().expect("Failed to fetch stderr"));
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
            .expect("Unexpected port string from backend")
            .1
            .trim()
            .parse::<u16>()
            .expect("Failed to parse port number");
        while let Err(_) = get(format!("http://127.0.0.1:{port}/")) {
            thread::sleep(Duration::from_millis(1));
            print!(".")
        }
        SalsaTestServer { process, port }
    }

    fn addr(&self) -> String {
        format!("http://127.0.0.1:{}/bookings", self.port)
    }
}

impl Drop for SalsaTestServer {
    fn drop(&mut self) {
        self.process
            .kill()
            .expect("Failed to send kill signal to backend");
        self.process.wait().expect("Backend failed to stop");
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
        .post(server.addr())
        .form(&[
            ("start_date", "2025-07-01"),
            ("start_time", "02:00:00"),
            ("telescope", "fake1"),
            ("duration", "1"),
        ])
        .send()
        .expect("Could not send request");

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
