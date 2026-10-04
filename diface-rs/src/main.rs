//! `diface` command line interface.

use std::path::PathBuf;
use std::process::ExitCode;

use diface_rs::{
    deface,
    viewer::{collect_output_files, launch_viewer},
    AtlasSpec, BackendKind, DefaceAlgorithm, DefaceOptions, DefacePreset, FillValue,
    GeometricParams, Threshold,
};

const USAGE: &str = "\
diface - facial anonymisation (defacing) for DICOM series

USAGE:
    diface [OPTIONS] <INPUT> <OUTPUT>

ARGS:
    <INPUT>     A DICOM file, or a directory of DICOM files
    <OUTPUT>    Output directory for defaced files

OPTIONS:
    --recursive                     Recurse into subdirectories of INPUT
    --backend <geometric|atlas|segmentation>
                                    Backend to use (default: geometric)
    --atlas <FILE>                  Use an atlas brain mask file (.dfatlas)
    --seg-region <face|deflesh>     Segmentation region: face cut or deflesh
                                    (remove external soft tissue/bone)
    --brain-protect <MM>            Segmentation: safety band kept around the
                                    intracranial core (default: 2)
    --deflesh-smooth <MM>           Deflesh: feather the removal boundary by MM
                                    (0 = hard cut; default: 3)
    --deflesh-posterior <MM|all>    Deflesh: keep external tissue posterior of
                                    brain centre + MM (0 = keep neck/back; all
                                    = remove all external tissue)
    --algorithm <ellipsoid|plane|curved>
                                    Masking algorithm (default: ellipsoid)
    --preset <conservative|balanced|thorough>
                                    Brain-safety preset (default: balanced)
    --threshold <auto|otsu|N>       Foreground threshold (default: auto)
    --preserve-fraction <F>         Preserve ellipsoid size, 0..1 (default: 0.82)
    --anterior-bias <F>             Shift preserve ellipsoid posteriorly (default: 0.05)
    --superior-bias <F>             Shift preserve ellipsoid superiorly (default: 0.05)
    --yaw <DEG>                     Rotate the head frame about superior (default: 0)
    --depth <MM>                    Anterior/posterior shift of the cut, mm (default: 0)
    --extent <A,L,S>                Preserve-ellipsoid scale per axis (default: 1,1,1)
    --fill <min|zero|N>             Value written into removed voxels (default: min)
    --min-slices <N>                Skip series with fewer slices (default: 3)
    --max-removed-fraction <F>      Safety cap on removed fraction (default: 0.6)
    --no-subdir                     Write all series into OUTPUT directly
    --dry-run                       Compute masks but write nothing
    --remove-original               Delete inputs after a successful write
    --view                          Open the defaced output in diviz-rs
    -h, --help                      Print this help
    -V, --version                   Print version

Viewing:
    With --view, the defaced slices are opened in diviz-rs. The viewer binary is
    searched on PATH, then under ./diviz-rs/target/{debug,release}/diviz-rs.
    Override with the DIVACE_VIEWER environment variable.

Backends:
    geometric    Threshold + principal-axis head geometry (no models)
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("diface: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();

    let mut opts = DefaceOptions::default();
    let mut params = GeometricParams::default();
    let mut view = false;
    let mut backend_choice = "geometric".to_string();
    let mut atlas_path: Option<std::path::PathBuf> = None;
    let mut seg_region = diface_rs::MaskRegion::Face;
    let mut deflesh_posterior_mm = 0.0f64;
    let mut brain_protect_mm = 2.0f64;
    let mut deflesh_smooth_mm = 3.0f64;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let take_value = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("missing value for {}", arg))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("diface {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--recursive" => opts.recursive = true,
            "--no-subdir" => opts.subdir_by_series = false,
            "--dry-run" => opts.dry_run = true,
            "--remove-original" => opts.remove_original = true,
            "--view" => view = true,
            "--backend" => {
                let v = take_value(&mut i)?;
                match v.to_ascii_lowercase().as_str() {
                    "geometric" => backend_choice = "geometric".to_string(),
                    "atlas" => backend_choice = "atlas".to_string(),
                    "segmentation" | "seg" => backend_choice = "segmentation".to_string(),
                    other => return Err(format!("unknown backend '{other}'")),
                }
            }
            "--atlas" => {
                let v = take_value(&mut i)?;
                atlas_path = Some(std::path::PathBuf::from(v));
                backend_choice = "atlas".to_string();
            }
            "--algorithm" => {
                let v = take_value(&mut i)?;
                params.algorithm = match v.to_ascii_lowercase().as_str() {
                    "ellipsoid" | "ellip" => DefaceAlgorithm::Ellipsoid,
                    "plane" => DefaceAlgorithm::Plane,
                    "curved" | "curved-front" | "curvedfront" => DefaceAlgorithm::CurvedFront,
                    other => return Err(format!("unknown --algorithm '{other}'")),
                };
            }
            "--preset" => {
                let v = take_value(&mut i)?;
                let preset = match v.to_ascii_lowercase().as_str() {
                    "conservative" => DefacePreset::Conservative,
                    "balanced" => DefacePreset::Balanced,
                    "thorough" => DefacePreset::Thorough,
                    other => return Err(format!("unknown --preset '{other}'")),
                };
                // Presets set sizing/bias; explicit flags below still override.
                params.apply_preset(preset);
            }
            "--seg-region" => {
                let v = take_value(&mut i)?;
                seg_region = match v.to_ascii_lowercase().as_str() {
                    "face" => diface_rs::MaskRegion::Face,
                    "deflesh" | "external" => diface_rs::MaskRegion::ExternalSoftTissue,
                    other => return Err(format!("unknown --seg-region '{other}'")),
                };
            }
            "--brain-protect" => {
                let v = take_value(&mut i)?;
                brain_protect_mm = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --brain-protect '{v}'"))?;
            }
            "--deflesh-smooth" => {
                let v = take_value(&mut i)?;
                deflesh_smooth_mm = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --deflesh-smooth '{v}'"))?;
            }
            "--deflesh-posterior" => {
                let v = take_value(&mut i)?;
                deflesh_posterior_mm = if v.eq_ignore_ascii_case("all") || v.eq_ignore_ascii_case("inf") {
                    f64::INFINITY
                } else {
                    v.parse::<f64>()
                        .map_err(|_| format!("invalid --deflesh-posterior '{v}'"))?
                };
            }
            "--threshold" => {
                let v = take_value(&mut i)?;
                params.threshold = match v.as_str() {
                    "auto" => Threshold::Auto,
                    "otsu" => Threshold::Otsu,
                    other => Threshold::Manual(
                        other
                            .parse::<f32>()
                            .map_err(|_| format!("invalid --threshold '{other}'"))?,
                    ),
                };
            }
            "--preserve-fraction" => {
                let v = take_value(&mut i)?;
                params.preserve_fraction = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --preserve-fraction '{v}'"))?;
            }
            "--anterior-bias" => {
                let v = take_value(&mut i)?;
                params.anterior_bias = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --anterior-bias '{v}'"))?;
            }
            "--superior-bias" => {
                let v = take_value(&mut i)?;
                params.superior_bias = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --superior-bias '{v}'"))?;
            }
            "--yaw" => {
                let v = take_value(&mut i)?;
                params.yaw_deg = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --yaw '{v}'"))?;
            }
            "--depth" => {
                let v = take_value(&mut i)?;
                params.anterior_offset_mm = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --depth '{v}'"))?;
            }
            "--extent" => {
                let v = take_value(&mut i)?;
                let parts: Vec<f64> = v
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<f64>()
                            .map_err(|_| format!("invalid --extent '{v}'"))
                    })
                    .collect::<Result<_, _>>()?;
                if parts.len() != 3 {
                    return Err(format!(
                        "--extent expects 3 comma-separated values, got '{v}'"
                    ));
                }
                params.extent_scale = [parts[0], parts[1], parts[2]];
            }
            "--max-removed-fraction" => {
                let v = take_value(&mut i)?;
                params.max_removed_fraction = v
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --max-removed-fraction '{v}'"))?;
            }
            "--min-slices" => {
                let v = take_value(&mut i)?;
                opts.min_slices = v
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --min-slices '{v}'"))?;
            }
            "--fill" => {
                let v = take_value(&mut i)?;
                opts.fill = match v.as_str() {
                    "min" => FillValue::Min,
                    "zero" => FillValue::Zero,
                    other => FillValue::Explicit(
                        other
                            .parse::<f32>()
                            .map_err(|_| format!("invalid --fill '{other}'"))?,
                    ),
                };
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'\n\n{USAGE}"));
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    if positional.len() != 2 {
        return Err(format!("expected <INPUT> and <OUTPUT>\n\n{USAGE}"));
    }

    opts.backend = if backend_choice == "atlas" {
        let spec = match atlas_path {
            Some(p) => AtlasSpec::File(p),
            None => AtlasSpec::Synthetic {
                dims: [128, 160, 140],
                spacing_mm: 1.0,
            },
        };
        BackendKind::Atlas(spec)
    } else if backend_choice == "segmentation" {
        BackendKind::Segmentation(diface_rs::SegBackendParams {
            region: seg_region,
            brain_protect_mm,
            deflesh_posterior_mm,
            deflesh_smooth_mm,
            seg: diface_rs::SegParams {
                threshold: params.threshold.clone(),
                ..diface_rs::SegParams::default()
            },
            ..diface_rs::SegBackendParams::default()
        })
    } else {
        BackendKind::Geometric(params)
    };

    let input = PathBuf::from(&positional[0]);
    let output = PathBuf::from(&positional[1]);

    let report = deface(&input, &output, &opts)?;

    for w in &report.warnings {
        eprintln!("warning: {w}");
    }

    let mut removed_total = 0usize;
    let mut voxels_total = 0usize;
    for s in &report.series {
        let pct = s.removed_fraction() * 100.0;
        removed_total += s.removed_voxels;
        voxels_total += s.total_voxels;
        match &s.output_dir {
            Some(dir) => println!(
                "{} [{}] {} slice(s): removed {} / {} voxels ({:.1}%) -> {}",
                s.uid,
                s.modality,
                s.slices,
                s.removed_voxels,
                s.total_voxels,
                pct,
                dir.display()
            ),
            None => println!(
                "{} [{}] {} slice(s): removed {} / {} voxels ({:.1}%) [dry-run]",
                s.uid, s.modality, s.slices, s.removed_voxels, s.total_voxels, pct
            ),
        }
    }

    if !report.series.is_empty() {
        let pct = if voxels_total == 0 {
            0.0
        } else {
            removed_total as f64 / voxels_total as f64 * 100.0
        };
        println!(
            "total: {} series, removed {} / {} voxels ({:.1}%)",
            report.series.len(),
            removed_total,
            voxels_total,
            pct
        );
    }

    if view {
        if opts.dry_run {
            eprintln!("warning: --view has no output to show in --dry-run mode");
        } else {
            let paths = collect_output_files(&report);
            match launch_viewer(&paths) {
                Ok(()) => println!("opened {} file(s) in diviz-rs", paths.len()),
                Err(e) => eprintln!("warning: could not open viewer: {e}"),
            }
        }
    }

    Ok(())
}
