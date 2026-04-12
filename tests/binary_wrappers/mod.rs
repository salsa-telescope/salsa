use reqwest::StatusCode;
use reqwest::blocking::get;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

pub struct SalsaTestServer {
    process: Child,
    port: u16,
    database_dir: TempDir,
}

pub struct LocalSalsaUser {
    pub username: String,
    pub password: String,
}

impl SalsaTestServer {
    pub fn spawn() -> Self {
        let database_dir = TempDir::new().expect("Need to be able to create tempdir in test");
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
            database_dir,
        }
    }

    pub fn addr(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn add_local_user(self: &Self, username: &str, password: &str) -> LocalSalsaUser {
        let manage_user_executable = env!("CARGO_BIN_EXE_manage_users");
        let output = Command::new(manage_user_executable)
            .args([
                "add-local",
                username,
                "--database-dir",
                self.database_dir
                    .path()
                    .to_str()
                    .expect("TempDir path should convert to str"),
            ])
            .env("PASSWORD", password)
            .output()
            .expect("Should be possible to add local user");

        if !output.status.success() {
            panic!(
                "Failed to add local user:\nstdout:{:?}\nstderr:{:?}",
                String::from_utf8(output.stdout).expect("stdout should be utf8"),
                String::from_utf8(output.stderr).expect("stderr should be utf8"),
            );
        }

        LocalSalsaUser {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    pub fn login(self: &Self, client: &reqwest::blocking::Client, user: &LocalSalsaUser) {
        let res = client
            .post(self.addr() + "/auth/local")
            .form(&[("username", &user.username), ("password", &user.password)])
            .send()
            .expect("Should be able to send request");
        assert_eq!(StatusCode::OK, res.status());
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

pub struct SimSalsaBin {
    process: Child,
    pub port: u16,
}

impl Drop for SimSalsaBin {
    fn drop(&mut self) {
        self.process
            .kill()
            .expect("Should be able to kill simulated salsa telescope");
        self.process
            .wait()
            .expect("Simulated salsa telescope should stop on kill signal");
    }
}

impl SimSalsaBin {
    pub fn spawn() -> Self {
        let backend_executable = env!("CARGO_BIN_EXE_simsalsabin");
        let mut process = Command::new(backend_executable)
            .args([
                "-p", // Let the OS decide the port
                "0",
            ])
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
            .expect("Simsalsabin should print something with \":\"")
            .1
            .trim()
            .parse::<u16>()
            .expect("Simsalsabin should print a number");

        // Let the thread detach.
        thread::spawn(|| {
            for line in stdout_reader.lines() {
                match line {
                    Ok(line) => print!("{line}"),
                    Err(_) => return,
                }
            }
        });

        SimSalsaBin { process, port }
    }
}
