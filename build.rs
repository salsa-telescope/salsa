use std::fs;
use std::path::Path;
use std::process::Command;

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

fn tailwind_binary_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "tailwindcss-linux-x64",
        ("linux", "aarch64") => "tailwindcss-linux-arm64",
        ("macos", "x86_64") => "tailwindcss-macos-x64",
        ("macos", "aarch64") => "tailwindcss-macos-arm64",
        (os, arch) => panic!("Unsupported platform: {os}/{arch}"),
    }
}

fn download_tailwind(path: &Path) {
    let url = format!(
        "https://github.com/tailwindlabs/tailwindcss/releases/download/{TAILWIND_VERSION}/{}",
        tailwind_binary_name()
    );
    eprintln!("Downloading Tailwind CSS {TAILWIND_VERSION}...");
    let status = Command::new("curl")
        .args(["-sL", &url, "-o"])
        .arg(path)
        .status()
        .expect("Failed to run curl");
    assert!(status.success(), "Failed to download Tailwind CSS");

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

    // Download if missing or wrong version
    let needs_download = if tailwind_bin.exists() {
        let output = Command::new(&tailwind_bin).arg("--version").output().ok();
        match output {
            Some(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                !stdout.contains(TAILWIND_VERSION.trim_start_matches('v'))
            }
            None => true,
        }
    } else {
        true
    };

    if needs_download {
        download_tailwind(&tailwind_bin);
    }

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
