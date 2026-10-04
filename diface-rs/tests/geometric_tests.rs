//! Tests for the geometric backend using synthetic volumes.

use diface_rs::geometric::{head_statistics, GeometricBackend, GeometricParams, Threshold};
use diface_rs::geometry::AXIS_SUPERIOR;
use diface_rs::volume::Volume;
use diface_rs::{
    compute_mask, deface_volume, BackendKind, DefaceOptions, DefacingBackend, FillValue,
};

/// A synthetic axial head: ellipsoid of tissue plus an anterior "nose".
fn synthetic_head() -> Volume {
    let nx = 64usize;
    let ny = 64usize;
    let nz = 72usize;
    let dims = [nx, ny, nz];
    let mut data = vec![-1000.0f32; nx * ny * nz];

    let half = |n: usize| n as f64 / 2.0;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;

    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let wx = x as f64 - half(nx);
                let wy = y as f64 - half(ny);
                let wz = z as f64 - half(nz);
                // Head: left/right 25mm, anterior/posterior 25mm, S-I 32mm.
                let e = (wx / 25.0).powi(2) + (wy / 25.0).powi(2) + (wz / 32.0).powi(2);
                let mut v = -1000.0f32;
                if e <= 1.0 {
                    v = 40.0;
                }
                // Nose protrudes anteriorly (negative Y).
                if wy <= -25.0 && wy >= -31.0 && wx.abs() <= 4.0 && wz.abs() <= 4.0 {
                    v = 40.0;
                }
                data[idx(x, y, z)] = v;
            }
        }
    }

    Volume {
        data,
        dims,
        spacing: [1.0, 1.0, 1.0],
        origin: [-half(nx), -half(ny), -half(nz)],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: vec![(1.0, 0.0); nz],
        modality: "CT".to_string(),
    }
}

/// A wide-in-plane, anisotropic, gantry-tilted axial stack resembling a "Head"
/// CT (0.5mm pixels, 5mm slices) whose in-plane field of view is larger than
/// the head. The head frame must still resolve to anatomical axes.
fn tilted_wide_fov_head() -> Volume {
    let nx = 128usize;
    let ny = 128usize;
    let nz = 24usize;
    let (cs, row_sp, slice_sp) = (0.5f64, 0.5f64, 5.0f64);
    // 15-degree gantry tilt about X: rows lean in -Z.
    let tilt = 15.0f64.to_radians();
    let col_dir = [1.0, 0.0, 0.0];
    let row_dir = [0.0, tilt.cos(), -tilt.sin()];
    let slice_dir = [
        row_dir[1] * 1.0 - row_dir[2] * 0.0,
        row_dir[2] * col_dir[0] - row_dir[0] * col_dir[2],
        row_dir[0] * 0.0 - row_dir[1] * col_dir[0],
    ];
    // Normalise the slice direction (cross(col, row)).
    let sn = (slice_dir[0].powi(2) + slice_dir[1].powi(2) + slice_dir[2].powi(2)).sqrt();
    let slice_dir = [slice_dir[0] / sn, slice_dir[1] / sn, slice_dir[2] / sn];

    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let mut data = vec![-1000.0f32; nx * ny * nz];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                // World point: origin at head centre, head radius 50mm, S-I 75mm.
                let wx = (x as f64 - nx as f64 / 2.0) * cs;
                let wy = (y as f64 - ny as f64 / 2.0) * row_sp;
                let wz = (z as f64 - nz as f64 / 2.0) * slice_sp;
                // Left-right 55mm, anterior-posterior 45mm, S-I 75mm - a real
                // head is not circular in-plane, so the axes are distinguishable.
                let e = (wx / 55.0).powi(2) + (wy / 45.0).powi(2) + (wz / 75.0).powi(2);
                data[idx(x, y, z)] = if e <= 1.0 { 40.0 } else { -1000.0 };
            }
        }
    }

    Volume {
        data,
        dims: [nx, ny, nz],
        spacing: [cs, row_sp, slice_sp],
        origin: [
            -(nx as f64 / 2.0) * cs,
            -(ny as f64 / 2.0) * row_sp,
            -(nz as f64 / 2.0) * slice_sp,
        ],
        dir: [col_dir, row_dir, slice_dir],
        rescale: vec![(1.0, 0.0); nz],
        modality: "CT".to_string(),
    }
}

#[test]
fn head_axes_resolve_on_tilted_wide_fov_stack() {
    let volume = tilted_wide_fov_head();
    let threshold = -300.0;
    let stats = head_statistics(&volume, threshold, 160).expect("stats");

    // Superior should follow the slice axis (the tilted normal), not drift into
    // an in-plane direction.
    let expected_sup = volume.dir[2];
    let dot_sup = stats.superior[0] * expected_sup[0]
        + stats.superior[1] * expected_sup[1]
        + stats.superior[2] * expected_sup[2];
    assert!(
        dot_sup.abs() > 0.9,
        "superior should align with the slice axis, got dot {dot_sup} ({:?})",
        stats.superior
    );

    // Anterior should point toward patient -Y (within the tilted plane).
    assert!(
        stats.anterior[1] < -0.5,
        "anterior should be predominantly -Y, got {:?}",
        stats.anterior
    );
}

#[test]
fn head_axes_resolve_on_anisotropic_isotropic_mix() {
    // Non-tilted but anisotropic and wide in-plane: superiority must still be Z.
    let mut volume = tilted_wide_fov_head();
    volume.dir = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let stats = head_statistics(&volume, -300.0, 160).expect("stats");
    assert!(
        stats.superior[2] > 0.9,
        "superior should be +Z, got {:?}",
        stats.superior
    );
    // For a symmetric ellipsoid the in-plane sign is arbitrary, but anterior
    // must be an in-plane (A-P) axis, i.e. perpendicular to superior.
    let dot_sa = stats.superior[0] * stats.anterior[0]
        + stats.superior[1] * stats.anterior[1]
        + stats.superior[2] * stats.anterior[2];
    assert!(
        dot_sa.abs() < 0.1,
        "anterior must be in-plane, dot={dot_sa}"
    );
    assert!(
        stats.anterior[1].abs() > 0.8,
        "anterior should be A-P, got {:?}",
        stats.anterior
    );
}

fn world_to_index(volume: &Volume, wx: f64, wy: f64, wz: f64) -> (usize, usize, usize) {
    let x = (wx - volume.origin[0]).round() as usize;
    let y = (wy - volume.origin[1]).round() as usize;
    let z = (wz - volume.origin[2]).round() as usize;
    (x, y, z)
}

/// Patient-space centroid of the removed voxels.
fn removed_centroid(volume: &Volume, mask: &diface_rs::volume::Mask) -> [f64; 3] {
    let (mut c, mut n) = ([0.0f64; 3], 0.0f64);
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if mask.is_removed(x, y, z) {
                    let w = volume.world(x, y, z);
                    for a in 0..3 {
                        c[a] += w[a];
                    }
                    n += 1.0;
                }
            }
        }
    }
    if n > 0.0 {
        for a in 0..3 {
            c[a] /= n;
        }
    }
    c
}

#[test]
fn manual_yaw_reorients_the_removal() {
    use diface_rs::geometric::HeadAxes;
    let volume = synthetic_head();

    // Baseline: automatic frame removes the face (anterior, -Y).
    let auto = GeometricBackend::new(GeometricParams::default())
        .compute_mask(&volume)
        .unwrap();
    let auto_c = removed_centroid(&volume, &auto);
    assert!(auto_c[1] < 0.0, "baseline removal is anterior (-Y)");

    // Yaw 90 degrees about superior: removal should swing to a lateral side.
    let yawed = GeometricBackend::new(GeometricParams {
        yaw_deg: 90.0,
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    let yaw_c = removed_centroid(&volume, &yawed);
    assert!(
        yaw_c[0].abs() > yaw_c[1].abs(),
        "after a 90-degree yaw removal should be lateral, got {yaw_c:?}"
    );

    // Explicit axis override: anterior = +X removes the +X surface.
    let override_mask = GeometricBackend::new(GeometricParams {
        axis_override: Some(HeadAxes {
            anterior: [1.0, 0.0, 0.0],
            left: [0.0, 1.0, 0.0],
            superior: [0.0, 0.0, 1.0],
        }),
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    let ov_c = removed_centroid(&volume, &override_mask);
    assert!(
        ov_c[0] > 0.0,
        "override anterior=+X removes the +X side, got {ov_c:?}"
    );
}

#[test]
fn algorithms_all_remove_the_face_and_spare_deep_brain() {
    use diface_rs::DefaceAlgorithm;
    let volume = synthetic_head();
    let nose = world_to_index(&volume, 0.0, -28.0, 0.0);
    let deep = world_to_index(&volume, 0.0, 10.0, 8.0);
    let posterior = world_to_index(&volume, 0.0, 24.0, 0.0);

    for algo in DefaceAlgorithm::all() {
        let mask = GeometricBackend::new(GeometricParams {
            algorithm: algo,
            ..GeometricParams::default()
        })
        .compute_mask(&volume)
        .expect("mask");

        assert!(mask.count_removed() > 0, "{algo:?} should remove something");
        assert!(
            mask.is_removed(nose.0, nose.1, nose.2),
            "{algo:?} should remove the nose"
        );
        assert!(
            !mask.is_removed(deep.0, deep.1, deep.2),
            "{algo:?} must preserve deep brain"
        );
        assert!(
            !mask.is_removed(posterior.0, posterior.1, posterior.2),
            "{algo:?} must preserve posterior tissue"
        );
    }
}

#[test]
fn curved_front_recedes_toward_the_vertex() {
    use diface_rs::DefaceAlgorithm;
    let volume = synthetic_head();
    let curved = GeometricBackend::new(GeometricParams {
        algorithm: DefaceAlgorithm::CurvedFront,
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();

    // A point at the top of the head, just anterior of the flat cut position,
    // should be preserved by the curved front because the cut recedes there.
    // (The ellipsoid/plane settings put the flat cut around a ~ +18mm.)
    let vertex_front = world_to_index(&volume, 0.0, -14.0, 30.0);
    assert!(
        !curved.is_removed(vertex_front.0, vertex_front.1, vertex_front.2),
        "curved front should spare tissue near the vertex"
    );

    // Plane and curved front must produce different masks.
    let plane = GeometricBackend::new(GeometricParams {
        algorithm: DefaceAlgorithm::Plane,
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    let mut differ = false;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if plane.is_removed(x, y, z) != curved.is_removed(x, y, z) {
                    differ = true;
                }
            }
        }
    }
    assert!(differ, "plane and curved-front masks must differ");
}

#[test]
fn preset_switch_keeps_the_algorithm() {
    use diface_rs::{DefaceAlgorithm, DefacePreset};
    let mut p = GeometricParams {
        algorithm: DefaceAlgorithm::CurvedFront,
        ..GeometricParams::default()
    };
    p.apply_preset(DefacePreset::Thorough);
    assert_eq!(
        p.algorithm,
        DefaceAlgorithm::CurvedFront,
        "apply_preset must not change the algorithm"
    );
}

#[test]
fn presets_order_by_brain_safety() {
    use diface_rs::DefacePreset;
    let volume = synthetic_head();

    let removed = |preset: DefacePreset| {
        GeometricBackend::from_preset(preset)
            .compute_mask(&volume)
            .unwrap()
            .count_removed()
    };
    let conservative = removed(DefacePreset::Conservative);
    let balanced = removed(DefacePreset::Balanced);
    let thorough = removed(DefacePreset::Thorough);

    assert!(
        conservative < balanced,
        "Conservative should remove less than Balanced ({conservative} vs {balanced})"
    );
    assert!(
        balanced < thorough,
        "Balanced should remove less than Thorough ({balanced} vs {thorough})"
    );

    // The default params are the Balanced preset.
    let default = GeometricBackend::new(GeometricParams::default())
        .compute_mask(&volume)
        .unwrap()
        .count_removed();
    assert_eq!(default, balanced, "default params should match Balanced");
    assert_eq!(
        GeometricParams::default().matching_preset(),
        Some(DefacePreset::Balanced)
    );
}

#[test]
fn applying_a_preset_keeps_manual_orientation() {
    use diface_rs::DefacePreset;
    let mut p = GeometricParams {
        yaw_deg: 33.0,
        ..GeometricParams::default()
    };
    p.apply_preset(DefacePreset::Thorough);
    assert_eq!(p.yaw_deg, 33.0, "preset must not reset manual yaw");
    assert_eq!(p.matching_preset(), Some(DefacePreset::Thorough));
}

#[test]
fn extent_scale_changes_removed_volume() {
    let volume = synthetic_head();
    let base = GeometricBackend::new(GeometricParams::default())
        .compute_mask(&volume)
        .unwrap();
    // A larger preserve ellipsoid (scale > 1) retains more, removing less.
    let wider = GeometricBackend::new(GeometricParams {
        extent_scale: [1.5, 1.5, 1.5],
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    let tighter = GeometricBackend::new(GeometricParams {
        extent_scale: [0.6, 0.6, 0.6],
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();

    assert!(
        wider.count_removed() < base.count_removed(),
        "larger extent should remove less"
    );
    assert!(
        tighter.count_removed() > base.count_removed(),
        "smaller extent should remove more"
    );
}

#[test]
fn anterior_offset_shifts_the_cut() {
    let volume = synthetic_head();
    let base = GeometricBackend::new(GeometricParams::default())
        .compute_mask(&volume)
        .unwrap();
    // Pushing the ellipsoid forward (positive anterior offset) removes more.
    let forward = GeometricBackend::new(GeometricParams {
        anterior_offset_mm: 15.0,
        ..GeometricParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    assert!(
        forward.count_removed() > base.count_removed(),
        "forward offset should remove more"
    );
}

#[test]
fn geometric_removes_nose_but_keeps_brain() {
    let volume = synthetic_head();
    let backend = GeometricBackend::new(GeometricParams::default());
    let mask = backend
        .compute_mask(&volume)
        .expect("mask should be produced");

    assert!(
        mask.count_removed() > 0,
        "some facial voxels should be removed"
    );

    // Nose tip, far anterior.
    let (x, y, z) = world_to_index(&volume, 0.0, -28.0, 0.0);
    assert!(
        mask.is_removed(x, y, z),
        "the protruding nose should be removed"
    );

    // Deep posterior brain tissue must be preserved.
    let (x, y, z) = world_to_index(&volume, 0.0, 10.0, 8.0);
    assert!(
        !mask.is_removed(x, y, z),
        "deep brain tissue must be preserved"
    );

    // Posterior head surface is not anterior, so it is preserved.
    let (x, y, z) = world_to_index(&volume, 0.0, 24.0, 0.0);
    assert!(
        !mask.is_removed(x, y, z),
        "posterior tissue should be preserved"
    );

    // The mask must stay well below the safety cap.
    let frac = mask.count_removed() as f64 / volume.len() as f64;
    assert!(frac < 0.5, "removed fraction {frac} unexpectedly high");
}

#[test]
fn head_statistics_finds_superior_axis() {
    let volume = synthetic_head();
    let stats = head_statistics(&volume, -300.0, 160).expect("stats");
    let dot_z = stats.superior[0] * AXIS_SUPERIOR[0]
        + stats.superior[1] * AXIS_SUPERIOR[1]
        + stats.superior[2] * AXIS_SUPERIOR[2];
    assert!(
        dot_z > 0.9,
        "principal axis should be superior, got {dot_z}"
    );
    // Centre should be near the volume centre.
    assert!(stats.centroid.iter().all(|c| c.abs() < 5.0));
}

#[test]
fn empty_volume_is_rejected() {
    let volume = Volume {
        data: Vec::new(),
        dims: [0, 0, 0],
        spacing: [1.0, 1.0, 1.0],
        origin: [0.0, 0.0, 0.0],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: Vec::new(),
        modality: "CT".to_string(),
    };
    let backend = GeometricBackend::new(GeometricParams::default());
    assert!(backend.compute_mask(&volume).is_err());
}

#[test]
fn deface_volume_fills_removed_with_minimum() {
    let mut volume = synthetic_head();
    let backend = GeometricBackend::new(GeometricParams::default());
    let removed =
        deface_volume(&mut volume, &backend, FillValue::Min).expect("deface should succeed");
    assert!(removed > 0);

    let (x, y, z) = world_to_index(&volume, 0.0, -28.0, 0.0);
    assert_eq!(
        volume.raw(x, y, z),
        -1000.0,
        "removed voxels should be blanked to the volume minimum"
    );
}

#[test]
fn compute_mask_accepts_trait_object() {
    let volume = synthetic_head();
    let backend = BackendKind::Geometric(GeometricParams {
        threshold: Threshold::Manual(-300.0),
        ..GeometricParams::default()
    })
    .build();
    let dynamic: &dyn diface_rs::DefacingBackend = backend.as_ref();
    let mask = compute_mask(&volume, dynamic).expect("mask");
    assert!(mask.count_removed() > 0);
}

#[test]
fn deface_options_defaults_are_sane() {
    let opts = DefaceOptions::default();
    assert!(opts.min_slices >= 1);
    assert!(opts.subdir_by_series);
    assert!(!opts.dry_run);
}
