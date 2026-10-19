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

    embed_windows_icon(&manifest_dir);
}

/// Embed the uploader icon into the Windows executable so Explorer, the
/// taskbar, Alt-Tab and pinned shortcuts show the app branding instead of the
/// default Rust/Cargo icon. This only affects Windows targets.
///
/// Build scripts always run on the *host*, so we must consult the compilation
/// target via `CARGO_CFG_TARGET_OS` rather than `cfg!(windows)` (which would be
/// false when cross-compiling to Windows from Linux/macOS).
fn embed_windows_icon(manifest_dir: &std::path::Path) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = manifest_dir.join("assets/generated/uploade-rs_icon.ico");
    if !icon.exists() {
        println!(
            "cargo:warning=Windows icon {} not found; the executable will have no icon",
            icon.display()
        );
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon path is not valid UTF-8"));
    if let Err(err) = res.compile() {
        panic!("failed to embed Windows icon resource: {err}");
    }
}
