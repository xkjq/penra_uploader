//! Tests for the atlas/template defacing backend.

use diface_rs::atlas::{Atlas, AtlasBackend, AtlasParams, AtlasSpec};
use diface_rs::backend::DefacingBackend;
use diface_rs::volume::Volume;

/// An axial head with a nose, in standard patient orientation (x=left,
/// y=posterior, z=superior), at 2mm isotropic, about 140mm cube.
fn synthetic_head() -> Volume {
    let n = 70usize;
    let spacing = 2.0f64;
    let dims = [n, n, n];
    let mut data = vec![-1000.0f32; n * n * n];
    let c = (n as f64 - 1.0) / 2.0;
    let idx = |x: usize, y: usize, z: usize| (z * n + y) * n + x;
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let wx = (x as f64 - c) * spacing;
                let wy = (y as f64 - c) * spacing;
                let wz = (z as f64 - c) * spacing;
                // Head: 60mm left/right, 75mm A-P, 80mm S-I.
                let e = (wx / 60.0).powi(2) + (wy / 75.0).powi(2) + (wz / 80.0).powi(2);
                let mut v = -1000.0f32;
                if e <= 1.0 {
                    v = 40.0;
                }
                // Nose protrudes anteriorly (negative Y).
                if wy <= -75.0 && wy >= -95.0 && wx.abs() <= 6.0 && wz.abs() <= 10.0 {
                    v = 40.0;
                }
                data[idx(x, y, z)] = v;
            }
        }
    }
    Volume {
        data,
        dims,
        spacing: [spacing, spacing, spacing],
        origin: [-c * spacing, -c * spacing, -c * spacing],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: vec![(1.0, 0.0); n],
        modality: "CT".to_string(),
    }
}

fn world_to_index(volume: &Volume, wx: f64, wy: f64, wz: f64) -> (usize, usize, usize) {
    (
        ((wx - volume.origin[0]) / volume.spacing[0]).round() as usize,
        ((wy - volume.origin[1]) / volume.spacing[1]).round() as usize,
        ((wz - volume.origin[2]) / volume.spacing[2]).round() as usize,
    )
}

#[test]
fn atlas_file_roundtrip() {
    let atlas = Atlas::synthetic([32, 40, 36], 1.5);
    let bytes = atlas.to_bytes();
    let back = Atlas::from_bytes(&bytes).expect("parse");
    assert_eq!(back.dims, atlas.dims);
    assert_eq!(back.brain, atlas.brain);
    assert!((back.spacing[0] - 1.5).abs() < 1e-4);

    // Corrupt magic is rejected.
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(Atlas::from_bytes(&bad).is_err());
    // Truncated body is rejected.
    assert!(Atlas::from_bytes(&bytes[..60]).is_err());
}

#[test]
fn atlas_spec_synthetic_and_file() {
    let spec = AtlasSpec::Synthetic {
        dims: [40, 50, 44],
        spacing_mm: 1.0,
    };
    assert_eq!(spec.name(), "atlas:synthetic");
    let atlas = spec.load().expect("load synthetic");
    assert_eq!(atlas.dims, [40, 50, 44]);
    assert!(
        atlas.brain.iter().any(|&b| b != 0),
        "synthetic brain non-empty"
    );

    // Round-trip via a file.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("brain.dfatlas");
    std::fs::write(&path, atlas.to_bytes()).unwrap();
    let from_file = AtlasSpec::File(path).load().expect("load file");
    assert_eq!(from_file.dims, atlas.dims);
}

#[test]
fn atlas_backend_removes_face_and_spares_brain() {
    let volume = synthetic_head();
    let atlas = Atlas::synthetic([128, 160, 140], 1.0);
    let backend = AtlasBackend::new(
        atlas,
        AtlasParams {
            brain_margin_mm: 10.0,
            ..AtlasParams::default()
        },
    );
    let mask = backend.compute_mask(&volume).expect("atlas mask");

    assert!(mask.count_removed() > 0, "should remove facial voxels");

    // Nose tip (far anterior) removed.
    let nose = world_to_index(&volume, 0.0, -88.0, 0.0);
    assert!(mask.is_removed(nose.0, nose.1, nose.2), "nose removed");

    // Deep brain preserved.
    let brain = world_to_index(&volume, 0.0, 0.0, 0.0);
    assert!(!mask.is_removed(brain.0, brain.1, brain.2), "brain spared");

    // Removal is predominantly anterior (this is a coarse registration, so we
    // assert the direction rather than an exact boundary).
    let (mut ant, mut post) = (0usize, 0usize);
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if mask.is_removed(x, y, z) {
                    if volume.world(x, y, z)[1] < 0.0 {
                        ant += 1;
                    } else {
                        post += 1;
                    }
                }
            }
        }
    }
    let total = (ant + post).max(1);
    assert!(
        ant * 100 / total >= 55,
        "removal should be predominantly anterior, got {ant}/{total}"
    );
}
