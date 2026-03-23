#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;

#[cfg(target_os = "linux")]
pub fn install_desktop_icon_and_file() {
    use std::fs;
    use std::io::Write;
    use std::process::Command;

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };

    // source icon embedded in the binary
    let icon_bytes: &[u8] = include_bytes!("../assets/generated/uploade-rs_icon_256.png");

    let icon_dest_dir = home.join(".local/share/icons/hicolor/256x256/apps");
    let desktop_dir = home.join(".local/share/applications");
    let icon_dest = icon_dest_dir.join("uploader_rs.png");
    let desktop_dest = desktop_dir.join("uploader_rs.desktop");

    if let Err(e) = fs::create_dir_all(&icon_dest_dir) {
        tracing::warn!("Failed to create icon dir: {}", e);
        return;
    }
    if let Err(e) = fs::create_dir_all(&desktop_dir) {
        tracing::warn!("Failed to create desktop dir: {}", e);
        return;
    }

    // Write icon file
    if let Err(e) = fs::write(&icon_dest, icon_bytes) {
        tracing::warn!("Failed to write icon file {}: {}", icon_dest.display(), e);
    } else {
        // Ensure readable
        let _ = fs::set_permissions(&icon_dest, fs::Permissions::from_mode(0o644));
        tracing::info!("Installed icon at {}", icon_dest.display());
    }

    // Exec path: try current exe
    let exec_path = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "uploader_rs".to_string());

    let desktop_contents = format!(r#"[Desktop Entry]
Version=1.0
Type=Application
Name=Uploader RS
Comment=Upload and anonymize DICOM studies
Exec={}
Icon=uploader_rs
Terminal=false
Categories=Utility;Graphics;
StartupWMClass=uploader_rs
"#, exec_path);

    if let Err(e) = fs::write(&desktop_dest, desktop_contents) {
        tracing::warn!("Failed to write .desktop file {}: {}", desktop_dest.display(), e);
    } else {
        let _ = fs::set_permissions(&desktop_dest, fs::Permissions::from_mode(0o644));
        tracing::info!("Wrote .desktop to {}", desktop_dest.display());
    }

    // Try to update desktop database (best-effort)
    match Command::new("update-desktop-database").arg(desktop_dir).status() {
        Ok(_) => tracing::info!("Ran update-desktop-database"),
        Err(_) => tracing::debug!("update-desktop-database not available"),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn install_desktop_icon_and_file() {
    // no-op on non-Linux
}
