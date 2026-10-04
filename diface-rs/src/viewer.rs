//! Optional integration with the `diviz-rs` viewer.
//!
//! This module only shells out to an existing viewer binary; it adds no GUI
//! dependencies to the library. It is used by the `diface` CLI `--view` flag
//! and can be reused by other callers (for example the uploader GUI).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::DefaceReport;

/// Collect the defaced output files referenced by a report.
pub fn collect_output_files(report: &DefaceReport) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for s in &report.series {
        if let Some(dir) = &s.output_dir {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_file() && is_viewable(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Is this file worth handing to the viewer?
pub fn is_viewable(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let e = ext.to_ascii_lowercase();
            e == "dcm" || e == "dicom" || e == "ima"
        }
        // Extensionless files are commonly DICOM; let the viewer decide.
        None => true,
    }
}

/// Candidate viewer locations under a repository root.
pub fn candidates_for_root(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for profile in ["debug", "release"] {
        out.push(root.join(format!("diviz-rs/target/{profile}/diviz-rs")));
        #[cfg(windows)]
        out.push(root.join(format!("diviz-rs/target/{profile}/diviz-rs.exe")));
    }
    out
}

/// Candidate viewer locations derived from a set of starting roots (their
/// ancestors are searched).
pub fn candidates_from_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let mut cur = Some(root.as_path());
        for _ in 0..8 {
            match cur {
                Some(p) => {
                    out.extend(candidates_for_root(p));
                    cur = p.parent();
                }
                None => break,
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.to_string_lossy().to_string()));
    out
}

/// Roots used by [`launch_viewer`] when no explicit candidates exist:
/// the current directory, the running executable and `CARGO_MANIFEST_DIR`.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        roots.push(PathBuf::from(manifest_dir));
    }
    roots
}

/// Launch the viewer with the given files.
///
/// Resolution order: the `DIVACE_VIEWER` environment variable, then `diviz-rs`
/// on `PATH`, then workspace-relative build outputs.
pub fn launch_viewer(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no output files to view".to_string());
    }
    let args: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let spawn = |cmd: &OsStr| -> std::io::Result<std::process::Child> {
        Command::new(cmd).args(&args).stdin(Stdio::null()).spawn()
    };

    if let Ok(over) = std::env::var("DIVACE_VIEWER") {
        if !over.is_empty() {
            return spawn(OsStr::new(&over))
                .map(|_| ())
                .map_err(|e| format!("DIVACE_VIEWER='{over}': {e}"));
        }
    }

    if spawn(OsStr::new("diviz-rs")).is_ok() {
        return Ok(());
    }

    for cand in candidates_from_roots(&default_roots()) {
        if cand.is_file() && spawn(cand.as_os_str()).is_ok() {
            return Ok(());
        }
    }

    Err(
        "diviz-rs not found; build it (`cd diviz-rs && cargo build --release`) \
         or set DIVACE_VIEWER"
            .to_string(),
    )
}
