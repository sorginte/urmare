use std::env;
use std::process::Command;

fn main() {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let rustc_version = command_output(Command::new(rustc).arg("--version"));
    println!("cargo:rustc-env=URMARE_BUILD_RUSTC_VERSION={rustc_version}");

    let git_commit = command_output(Command::new("git").args(["rev-parse", "HEAD"]));
    println!("cargo:rustc-env=URMARE_BUILD_GIT_COMMIT={git_commit}");

    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Ok(reference) = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        && reference.status.success()
    {
        let reference = String::from_utf8_lossy(&reference.stdout);
        println!("cargo:rerun-if-changed=../../.git/{}", reference.trim());
    }
}

fn command_output(command: &mut Command) -> String {
    command
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|output| !output.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
