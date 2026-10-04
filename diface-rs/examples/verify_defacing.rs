//! Verify that a defaced series matches the mask the backend would produce.
//!
//! ```bash
//! cargo run --example verify_defacing -- <original_dir> <defaced_dir>
//! ```
//!
//! Checks:
//! - every changed voxel was set to the fill value,
//! - the set of changed voxels exactly equals a freshly computed mask,
//! - prints ASCII previews of axial slices through the head (top = patient
//!   anterior for the usual HFS axial acquisition), where `#` marks removed
//!   voxels, `.` retained tissue and ` ` air.

use std::collections::HashMap;
use std::path::Path;

use diface_rs::geometric::{
    choose_threshold, head_statistics, GeometricBackend, GeometricParams, Threshold,
};
use diface_rs::series::{collect_dicom_files, load_series, SliceEntry};
use diface_rs::volume::Volume;
use diface_rs::DefacingBackend;

fn load(path: &str) -> Result<(Volume, Vec<SliceEntry>), String> {
    let files = collect_dicom_files(Path::new(path), true)?;
    load_series(&files)
}

fn name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

fn preview(orig: &Volume, mask: &diface_rs::volume::Mask, z: usize, threshold: f32, min_raw: f32) {
    let (nx, ny) = (orig.nx(), orig.ny());
    let cols = 64usize;
    let rows = 32usize;
    let sx = (nx as f64 / cols as f64).max(1.0);
    let sy = (ny as f64 / rows as f64).max(1.0);
    println!("\n--- axial slice z={z}  (# removed, . tissue, ' ' air) ---");
    for r in 0..rows {
        let mut line = String::with_capacity(cols);
        for c in 0..cols {
            let x = (((c as f64 + 0.5) * sx) as usize).min(nx - 1);
            let y = (((r as f64 + 0.5) * sy) as usize).min(ny - 1);
            if mask.is_removed(x, y, z) {
                line.push('#');
            } else if (orig.raw(x, y, z) - min_raw).abs() < 1.0 {
                line.push(' ');
            } else if orig.value(x, y, z) > threshold {
                line.push('.');
            } else {
                line.push(' ');
            }
        }
        println!("{line}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: verify_defacing <original_dir> <defaced_dir>");
        std::process::exit(1);
    }
    let (orig, oe) = load(&args[0]).expect("load original");
    let (defaced, de) = load(&args[1]).expect("load defaced");
    if orig.dims != defaced.dims {
        eprintln!("dimension mismatch: {:?} vs {:?}", orig.dims, defaced.dims);
        std::process::exit(1);
    }

    let mut def_index: HashMap<String, usize> = HashMap::new();
    for (i, e) in de.iter().enumerate() {
        def_index.insert(name(&e.path), i);
    }

    let backend = GeometricBackend::new(GeometricParams::default());
    let mask = backend.compute_mask(&orig).expect("recompute mask");
    let threshold = choose_threshold(&orig, &Threshold::Auto);
    let stats = head_statistics(&orig, threshold, 160).expect("head stats");
    let (min_raw, max_raw) = orig.raw_min_max();

    let nx = orig.nx();
    let ny = orig.ny();
    let nxy = nx * ny;

    let mut total_changed = 0usize;
    let mut mask_mismatch = 0usize;
    let mut fill_wrong = 0usize;
    let mut fill_value = f32::NAN;
    let mut removed_anterior = 0usize;
    let mut removed_posterior = 0usize;

    for (z, e) in oe.iter().enumerate() {
        let Some(&dz) = def_index.get(&name(&e.path)) else {
            eprintln!("missing defaced counterpart for {}", name(&e.path));
            continue;
        };
        let (obase, dbase) = (z * nxy, dz * nxy);
        for yi in 0..ny {
            for xi in 0..nx {
                let o = orig.data[obase + yi * nx + xi];
                let d = defaced.data[dbase + yi * nx + xi];
                let changed = (o - d).abs() > 0.5;
                if changed != mask.is_removed(xi, yi, z) {
                    mask_mismatch += 1;
                }
                if changed {
                    total_changed += 1;
                    if fill_value.is_nan() {
                        fill_value = d;
                    } else if (d - fill_value).abs() > 0.5 {
                        fill_wrong += 1;
                    }
                    let w = orig.world(xi, yi, z);
                    let a = (w[0] - stats.centroid[0]) * stats.anterior[0]
                        + (w[1] - stats.centroid[1]) * stats.anterior[1]
                        + (w[2] - stats.centroid[2]) * stats.anterior[2];
                    if a > 0.0 {
                        removed_anterior += 1;
                    } else {
                        removed_posterior += 1;
                    }
                }
            }
        }
    }

    println!(
        "original : {}  ({}x{}x{})  modality={}",
        args[0],
        nx,
        ny,
        orig.nz(),
        orig.modality
    );
    println!("defaced  : {}  ({} slices)", args[1], de.len());
    println!(
        "threshold={threshold:.1}  raw range=[{min_raw:.0}, {max_raw:.0}]  fill={fill_value:.1}"
    );
    println!(
        "head half-extents (mm) A/L/S = [{:.0}, {:.0}, {:.0}]",
        stats.half[0], stats.half[1], stats.half[2]
    );
    println!(
        "removed voxels: {total_changed}   mask match: {}   wrong-fill: {fill_wrong}",
        if mask_mismatch == 0 { "OK" } else { "MISMATCH" }
    );
    println!(
        "removed relative to head centre: anterior {:.1}%  posterior {:.1}%",
        100.0 * removed_anterior as f64 / total_changed.max(1) as f64,
        100.0 * removed_posterior as f64 / total_changed.max(1) as f64
    );

    // Preview a few slices from lower brain to upper brain.
    for frac in [0.40, 0.55, 0.70] {
        let z = ((orig.nz() as f64 * frac) as usize).min(orig.nz() - 1);
        preview(&orig, &mask, z, threshold, min_raw);
    }
}
