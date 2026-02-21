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

fn main() {
    let git_branch_name = get_git_branch_name().unwrap_or("-".to_string());
    println!("cargo:rustc-env=GIT_BRANCH_NAME={git_branch_name}");

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
