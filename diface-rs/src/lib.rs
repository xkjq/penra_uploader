//! # diface-rs
//!
//! Facial anonymisation ("defacing") of DICOM series.
//!
//! `diface-rs` loads a multi-file grayscale DICOM series (CT or MR) as a 3D
//! volume, asks a pluggable [`DefacingBackend`] which voxels contain facial
//! anatomy, and writes the series back with those voxels blanked. Metadata is
//! left untouched so that this crate can run *before* a metadata anonymiser
//! such as `dicor-rs`.
//!
//! ## Quick start
//!
//! ```no_run
//! use diface_rs::{deface, DefaceOptions};
//! use std::path::Path;
//!
//! let report = deface(
//!     Path::new("incoming/series"),
//!     Path::new("defaced"),
//!     &DefaceOptions::default(),
//! )?;
//! for s in &report.series {
//!     println!("{}: removed {} / {} voxels", s.uid, s.removed_voxels, s.total_voxels);
//! }
//! # Ok::<(), String>(())
//! ```
//!
//! ## Backends
//!
//! The default [`geometric::GeometricBackend`] is fast, offline and requires
//! no models or external tools, at the cost of being conservative. Richer
//! strategies can implement [`DefacingBackend`] and be selected through
//! [`BackendKind`].

pub mod atlas;
pub mod backend;
pub mod geometric;
pub mod geometry;
pub mod segmentation;
pub mod series;
pub mod viewer;
pub mod volume;

use std::path::{Path, PathBuf};

pub use atlas::{Atlas, AtlasBackend, AtlasParams, AtlasSpec};
pub use backend::{BackendKind, DefacingBackend};
pub use geometric::{DefaceAlgorithm, DefacePreset, GeometricBackend, GeometricParams, Threshold};
pub use segmentation::{
    segment, CutReference, MaskRegion, SegBackendParams, SegParams, Segmentation,
    SegmentationBackend,
};
pub use series::{SeriesGroup, SliceEntry};
pub use volume::{Mask, Volume};

/// How removed voxels are filled.
#[derive(Clone, Copy, Debug)]
pub enum FillValue {
    /// Use the minimum raw stored value across the whole series (usually air).
    Min,
    /// Use zero in the stored pixel domain.
    Zero,
    /// Use an explicit raw stored value.
    Explicit(f32),
}

impl Default for FillValue {
    fn default() -> Self {
        FillValue::Min
    }
}

/// Options controlling a [`deface`] run.
#[derive(Clone, Debug)]
pub struct DefaceOptions {
    /// The defacing backend to use.
    pub backend: BackendKind,
    /// Value written into removed voxels.
    pub fill: FillValue,
    /// Recurse into subdirectories when discovering input files.
    pub recursive: bool,
    /// Series with fewer slices than this are skipped (they cannot be reliably
    /// reconstructed as a volume).
    pub min_slices: usize,
    /// Write each series into its own subdirectory of the output root.
    pub subdir_by_series: bool,
    /// Only compute and report the mask; write nothing.
    pub dry_run: bool,
    /// Delete the original input files after a successful write.
    pub remove_original: bool,
}

impl Default for DefaceOptions {
    fn default() -> Self {
        DefaceOptions {
            backend: BackendKind::default(),
            fill: FillValue::default(),
            recursive: false,
            min_slices: 3,
            subdir_by_series: true,
            dry_run: false,
            remove_original: false,
        }
    }
}

/// Result for a single series.
#[derive(Clone, Debug)]
pub struct SeriesReport {
    pub uid: String,
    pub modality: String,
    pub slices: usize,
    pub removed_voxels: usize,
    pub total_voxels: usize,
    /// Output directory, absent for dry runs or skipped series.
    pub output_dir: Option<PathBuf>,
}

impl SeriesReport {
    pub fn removed_fraction(&self) -> f64 {
        if self.total_voxels == 0 {
            0.0
        } else {
            self.removed_voxels as f64 / self.total_voxels as f64
        }
    }
}

/// Aggregate result of a [`deface`] run.
#[derive(Clone, Debug, Default)]
pub struct DefaceReport {
    pub series: Vec<SeriesReport>,
    /// Non-fatal problems (skipped files/series, etc.).
    pub warnings: Vec<String>,
}

/// Compute a removal mask for a volume with the given backend.
pub fn compute_mask(volume: &Volume, backend: &dyn DefacingBackend) -> Result<Mask, String> {
    backend.compute_mask(volume)
}

/// Deface every series found under `input`, writing into `output`.
///
/// Files are grouped by `SeriesInstanceUID`; each series is reconstructed and
/// processed independently.
pub fn deface(input: &Path, output: &Path, opts: &DefaceOptions) -> Result<DefaceReport, String> {
    let files = series::collect_dicom_files(input, opts.recursive)?;
    if files.is_empty() {
        return Err(format!("no DICOM files found under {}", input.display()));
    }

    let groups = series::group_by_series(&files)?;
    let backend = opts.backend.build();
    let mut report = DefaceReport::default();

    for group in groups {
        if group.files.len() < opts.min_slices {
            report.warnings.push(format!(
                "series {} has only {} slice(s); skipping (min {})",
                group.uid,
                group.files.len(),
                opts.min_slices
            ));
            continue;
        }

        let (mut volume, entries) = match series::load_series(&group.files) {
            Ok(v) => v,
            Err(e) => {
                report.warnings.push(format!("series {}: {}", group.uid, e));
                continue;
            }
        };

        let mask = match backend.compute_mask(&volume) {
            Ok(m) => m,
            Err(e) => {
                report.warnings.push(format!("series {}: {}", group.uid, e));
                continue;
            }
        };

        let removed = mask.count_removed();
        let modality = volume.modality.clone();

        let output_dir = if opts.dry_run {
            None
        } else {
            let dir = if opts.subdir_by_series {
                output.join(&group.uid)
            } else {
                output.to_path_buf()
            };
            let fill_raw = match opts.fill {
                FillValue::Min => volume.raw_min_max().0,
                FillValue::Zero => 0.0,
                FillValue::Explicit(v) => v,
            };
            match series::write_defaced_series(&entries, &volume, &mask, fill_raw, &dir) {
                Ok(n) => {
                    if n != entries.len() {
                        report.warnings.push(format!(
                            "series {}: wrote {} of {} slices",
                            group.uid,
                            n,
                            entries.len()
                        ));
                    }
                    if opts.remove_original {
                        for e in &entries {
                            let _ = std::fs::remove_file(&e.path);
                        }
                    }
                    Some(dir)
                }
                Err(e) => {
                    report.warnings.push(format!("series {}: {}", group.uid, e));
                    None
                }
            }
        };

        volume.data.clear();
        drop(volume);

        report.series.push(SeriesReport {
            uid: group.uid,
            modality,
            slices: entries.len(),
            removed_voxels: removed,
            total_voxels: mask.remove.len(),
            output_dir,
        });
    }

    if report.series.is_empty() && report.warnings.is_empty() {
        return Err("no processable series found".to_string());
    }
    Ok(report)
}

/// Deface every series found in `dir`, overwriting each file **in place**.
///
/// This is the integration entry point for pipelines that have already
/// anonymised/rewritten a directory (e.g. `uploader_rs`) and want to blank
/// facial voxels without moving files. Filenames are preserved, so callers can
/// keep any per-path bookkeeping (such as a cached pixel hash of the original
/// pixels) keyed by output path.
///
/// Returns a report describing each processed series.
pub fn deface_dir_in_place(dir: &Path, opts: &DefaceOptions) -> Result<DefaceReport, String> {
    let files = series::collect_dicom_files(dir, opts.recursive)?;
    if files.is_empty() {
        return Err(format!("no DICOM files found under {}", dir.display()));
    }

    let groups = series::group_by_series(&files)?;
    let backend = opts.backend.build();
    let mut report = DefaceReport::default();

    for group in groups {
        if group.files.len() < opts.min_slices {
            report.warnings.push(format!(
                "series {} has only {} slice(s); skipping (min {})",
                group.uid,
                group.files.len(),
                opts.min_slices
            ));
            continue;
        }

        let (mut volume, entries) = match series::load_series(&group.files) {
            Ok(v) => v,
            Err(e) => {
                report.warnings.push(format!("series {}: {}", group.uid, e));
                continue;
            }
        };

        let mask = match backend.compute_mask(&volume) {
            Ok(m) => m,
            Err(e) => {
                report.warnings.push(format!("series {}: {}", group.uid, e));
                continue;
            }
        };

        let removed = mask.count_removed();
        let modality = volume.modality.clone();
        let fill_raw = match opts.fill {
            FillValue::Min => volume.raw_min_max().0,
            FillValue::Zero => 0.0,
            FillValue::Explicit(v) => v,
        };

        // Write each slice back over the directory it came from.
        let mut output_dir = None;
        if !opts.dry_run {
            let mut ok = true;
            for entry in &entries {
                let parent = entry.path.parent().unwrap_or(dir);
                match series::write_defaced_series(
                    std::slice::from_ref(entry),
                    &volume,
                    &mask,
                    fill_raw,
                    parent,
                ) {
                    Ok(n) if n == 1 => {}
                    Ok(_) => ok = false,
                    Err(e) => {
                        report.warnings.push(format!("series {}: {}", group.uid, e));
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                output_dir = entries
                    .first()
                    .and_then(|e| e.path.parent())
                    .map(Path::to_path_buf);
            }
        }

        volume.data.clear();
        drop(volume);

        report.series.push(SeriesReport {
            uid: group.uid,
            modality,
            slices: entries.len(),
            removed_voxels: removed,
            total_voxels: mask.remove.len(),
            output_dir,
        });
    }

    if report.series.is_empty() && report.warnings.is_empty() {
        return Err("no processable series found".to_string());
    }
    Ok(report)
}

/// Convenience wrapper for a single already-loaded volume: compute the mask
/// and blank the removed voxels in place. Returns the number removed.
pub fn deface_volume(
    volume: &mut Volume,
    backend: &dyn DefacingBackend,
    fill: FillValue,
) -> Result<usize, String> {
    let mask = backend.compute_mask(volume)?;
    let fill_raw = match fill {
        FillValue::Min => volume.raw_min_max().0,
        FillValue::Zero => 0.0,
        FillValue::Explicit(v) => v,
    };
    let mut removed = 0usize;
    for (i, r) in mask.remove.iter().enumerate() {
        if *r != 0 {
            volume.data[i] = fill_raw;
            removed += 1;
        }
    }
    Ok(removed)
}
