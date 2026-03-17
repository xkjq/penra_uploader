use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Build diviz-rs if its binary does not already exist for this profile.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let project_root = manifest_dir.parent().unwrap_or(&manifest_dir);
    let diviz_manifest = project_root.join("diviz-rs/Cargo.toml");

    let target_bin = project_root.join(format!("diviz-rs/target/{}/diviz-rs", profile));
    if target_bin.exists() {
        println!("cargo:warning=diviz-rs binary already exists at {}", target_bin.display());
        return;
    }

    if !diviz_manifest.exists() {
        println!("cargo:warning=diviz-rs manifest not found at {}; skipping build", diviz_manifest.display());
        return;
    }

    println!("cargo:warning=Building diviz-rs ({})...", profile);
    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.arg("--manifest-path");
    cmd.arg(diviz_manifest);
    if profile == "release" {
        cmd.arg("--release");
    }
    // inherit stdio so build output is visible when running cargo
    let status = cmd.status().expect("failed to launch cargo to build diviz-rs");
    if !status.success() {
        panic!("Failed to build diviz-rs (status {})", status);
    }
}
