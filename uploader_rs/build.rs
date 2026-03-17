use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Build auxiliary crates if their manifests exist. This ensures
    // viewer and helper crates are available when running the GUI.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let project_root = manifest_dir.parent().unwrap_or(&manifest_dir);

    let crates_to_build = ["diviz-rs", "dicor-rs", "divue-rs"];
    for crate_name in crates_to_build.iter() {
        let manifest = project_root.join(format!("{}/Cargo.toml", crate_name));
        if !manifest.exists() {
            println!("cargo:warning={} manifest not found at {}; skipping build", crate_name, manifest.display());
            continue;
        }

        println!("cargo:warning=Building {} ({})...", crate_name, profile);
        let mut cmd = Command::new("cargo");
        cmd.arg("build");
        cmd.arg("--manifest-path");
        cmd.arg(&manifest);
        if profile == "release" {
            cmd.arg("--release");
        }
        // inherit stdio so build output is visible when running cargo
        let status = cmd.status().expect(&format!("failed to launch cargo to build {}", crate_name));
        if !status.success() {
            panic!("Failed to build {} (status {})", crate_name, status);
        }
    }
}
