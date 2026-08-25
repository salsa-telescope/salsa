use std::fs;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

fn get_git_branch_name() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        let git_hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
        Some(git_hash)
    } else {
        None
    }
}

const TAILWIND_VERSION: &str = "v4.2.1";

/// Binary name plus its SHA-256, taken from the release's sha256sums.txt.
/// Pinning the hash means a tampered or truncated download fails the build
/// instead of being executed. Bumping TAILWIND_VERSION means updating these.
fn tailwind_binary() -> (&'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => (
            "tailwindcss-linux-x64",
            "39e8d4e24b3c83b0a6e69e100a972fbc75d5fef8dce47b3ddac3cf92dea81fe3",
        ),
        ("linux", "aarch64") => (
            "tailwindcss-linux-arm64",
            "d87e6486bb3f70b04ef1dcaacc4ee6548a5a15fbf521b31bc24d2c774f68a951",
        ),
        ("macos", "x86_64") => (
            "tailwindcss-macos-x64",
            "019e5cfa441992ede2772c6faaeb8d7fb1726aab50b1138c0aa38e88f4b7bd44",
        ),
        ("macos", "aarch64") => (
            "tailwindcss-macos-arm64",
            "e510af7928750c9ee8d5ff2e5e98088bd5b99a8a8e2c554668621c7e151fa91f",
        ),
        (os, arch) => panic!("Unsupported platform: {os}/{arch}"),
    }
}

fn sha256_of(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

fn download_tailwind(path: &Path) {
    let (name, expected) = tailwind_binary();
    let url = format!(
        "https://github.com/tailwindlabs/tailwindcss/releases/download/{TAILWIND_VERSION}/{name}"
    );
    eprintln!("Downloading Tailwind CSS {TAILWIND_VERSION}...");
    let status = Command::new("curl")
        .args(["-sSfL", &url, "-o"])
        .arg(path)
        .status()
        .expect("Failed to run curl");
    assert!(status.success(), "Failed to download Tailwind CSS");

    let actual = sha256_of(path).expect("Failed to read downloaded Tailwind CSS");
    if actual != expected {
        let _ = fs::remove_file(path);
        panic!(
            "Tailwind CSS {TAILWIND_VERSION} checksum mismatch: expected {expected}, got {actual}"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("Failed to set permissions");
    }
}

fn build_tailwind() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_dir = Path::new(&manifest_dir);
    let tailwind_bin = manifest_dir.join("tailwindcss");

    // Download if missing, the wrong version, or not the binary we pinned
    let (_, expected) = tailwind_binary();
    let needs_download = sha256_of(&tailwind_bin).is_none_or(|actual| actual != expected);

    if needs_download {
        download_tailwind(&tailwind_bin);
    }

    // The binary now matches the pinned hash, so it is safe to run. Its version
    // is not implied by that hash, though: bumping TAILWIND_VERSION without
    // updating the hashes would silently keep using the old binary.
    let output = Command::new(&tailwind_bin)
        .arg("--version")
        .output()
        .expect("Failed to run tailwindcss --version");
    let reported = String::from_utf8_lossy(&output.stdout);
    assert!(
        reported.contains(TAILWIND_VERSION.trim_start_matches('v')),
        "Pinned hashes are stale: TAILWIND_VERSION is {TAILWIND_VERSION} but the binary reports \
         \"{}\". Update tailwind_binary() from that release's sha256sums.txt.",
        reported.trim()
    );

    let src = manifest_dir.join("assets/style.src.css");
    let out = manifest_dir.join("assets/style.css");
    let status = Command::new(&tailwind_bin)
        .args(["-i", &src.to_string_lossy(), "-o", &out.to_string_lossy()])
        .status()
        .expect("Failed to run tailwindcss");
    assert!(status.success(), "Tailwind CSS build failed");
}

fn main() {
    let git_branch_name = get_git_branch_name().unwrap_or("-".to_string());
    println!("cargo:rustc-env=GIT_BRANCH_NAME={git_branch_name}");

    build_tailwind();

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=assets/style.src.css");
    println!("cargo:rerun-if-changed=assets/");
    println!("cargo:rerun-if-changed=templates/");
}
