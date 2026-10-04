//! Tests for the in-house head/brain segmentation.

use diface_rs::backend::DefacingBackend;
use diface_rs::segmentation::{segment, SegBackendParams, SegParams, SegmentationBackend};
use diface_rs::volume::Volume;

/// Build a head volume with an attached neck/table, in standard orientation
/// (x=left, y=posterior, z=superior).
fn head_with_table() -> Volume {
    let (nx, ny, nz) = (80usize, 80usize, 80usize);
    let spacing = 2.0f64;
    let c = (nx as f64 - 1.0) / 2.0;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let mut data = vec![-1000.0f32; nx * ny * nz];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let wx = (x as f64 - c) * spacing;
                let wy = (y as f64 - c) * spacing;
                let wz = (z as f64 - c) * spacing;
                // Head: ellipsoid with a nose (anterior, -Y). Give it a bone
                // skull shell (high HU) so the CT brain region-grow is bounded,
                // as it would be in a real CT.
                let e = (wx / 55.0).powi(2) + (wy / 65.0).powi(2) + (wz / 70.0).powi(2);
                let mut v = -1000.0f32;
                if e <= 1.0 {
                    // Soft tissue inside; a thin bone shell near the surface.
                    v = 40.0;
                    if e > 0.72 {
                        v = 900.0; // skull
                    }
                }
                // Nose protrudes anteriorly (soft tissue, outside the skull).
                if wy <= -65.0 && wy >= -85.0 && wx.abs() <= 6.0 && wz.abs() <= 10.0 {
                    v = 40.0;
                }
                // Table: a long thin bar in +Y (posterior) across the bottom.
                if wy >= 60.0 && wy <= 90.0 && wz <= -55.0 {
                    v = 200.0;
                }
                data[idx(x, y, z)] = v;
            }
        }
    }
    Volume {
        data,
        dims: [nx, ny, nz],
        spacing: [spacing, spacing, spacing],
        origin: [-c * spacing, -c * spacing, -c * spacing],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: vec![(1.0, 0.0); nz],
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
fn segmentation_finds_head_and_brain() {
    let volume = head_with_table();
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    assert!(seg.head_count() > 0, "head found");
    assert!(seg.brain_count() > 0, "brain found");
    // Brain is a subset of the head (same-or-fewer cells).
    assert!(seg.brain_count() <= seg.head_count());
    // Brain centroid is near the volume centre.
    assert!(
        seg.brain_centroid[0].abs() < 20.0
            && seg.brain_centroid[1].abs() < 25.0
            && seg.brain_centroid[2].abs() < 25.0,
        "brain centroid near centre, got {:?}",
        seg.brain_centroid
    );
}

#[test]
fn segmentation_backend_removes_face_and_spares_brain() {
    let volume = head_with_table();
    let backend = SegmentationBackend::new(SegBackendParams {
        brain_margin_mm: 8.0,
        ..SegBackendParams::default()
    });
    let mask = backend.compute_mask(&volume).expect("mask");
    assert!(mask.count_removed() > 0);

    // Nose removed (probe a solid part of the protruding nose).
    let nose = world_to_index(&volume, 0.0, -70.0, 0.0);
    assert!(mask.is_removed(nose.0, nose.1, nose.2), "nose removed");

    // Deep brain spared.
    let brain = world_to_index(&volume, 0.0, 0.0, 0.0);
    assert!(!mask.is_removed(brain.0, brain.1, brain.2), "brain spared");

    // Removal is predominantly anterior.
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

#[test]
fn segmentation_tolerates_a_small_skull_gap() {
    // Same head, but punch a small hole in the bone shell. Without skull
    // closing the soft-tissue grow would leak out through the gap.
    let (nx, ny, nz) = (80usize, 80usize, 80usize);
    let spacing = 2.0f64;
    let c = (nx as f64 - 1.0) / 2.0;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let mut data = vec![-1000.0f32; nx * ny * nz];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let wx = (x as f64 - c) * spacing;
                let wy = (y as f64 - c) * spacing;
                let wz = (z as f64 - c) * spacing;
                let e = (wx / 55.0).powi(2) + (wy / 65.0).powi(2) + (wz / 70.0).powi(2);
                let mut v = -1000.0f32;
                if e <= 1.0 {
                    v = 40.0;
                    if e > 0.72 {
                        v = 900.0;
                    }
                }
                data[idx(x, y, z)] = v;
            }
        }
    }
    // Punch a 2x2-voxel hole in the shell on the anterior surface.
    for dz in 0..2 {
        for dy in 0..2 {
            let x = c as usize + dz;
            let y = (c as usize).saturating_sub(34) + dy;
            let z = c as usize;
            data[idx(x, y, z)] = 40.0;
        }
    }
    let volume = Volume {
        data,
        dims: [nx, ny, nz],
        spacing: [spacing, spacing, spacing],
        origin: [-c * spacing, -c * spacing, -c * spacing],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: vec![(1.0, 0.0); nz],
        modality: "CT".to_string(),
    };

    let backend = SegmentationBackend::new(SegBackendParams {
        brain_margin_mm: 8.0,
        ..SegBackendParams::default()
    });
    let mask = backend.compute_mask(&volume).expect("mask");
    // Deep brain preserved despite the skull gap.
    let brain = world_to_index(&volume, 0.0, 0.0, 0.0);
    assert!(!mask.is_removed(brain.0, brain.1, brain.2), "brain spared");
}

#[test]
fn segmented_head_stats_are_head_shaped() {
    // A head with a long posterior table bar. The plain principal-axis estimate
    // is pulled toward the table; the segmented estimate should keep the head's
    // superior axis near +Z and give head-scale extents.
    let mut volume = head_with_table();
    // Add a long, thin table bar far posterior and inferior (already present),
    // plus extend it to be clearly dominant in +Y.
    let dims = volume.dims;
    let ny = dims[1];
    let nx = dims[0];
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    for z in 0..3 {
        for x in 0..(nx / 4) {
            for y in (ny - 6)..ny {
                volume.data[idx(x, y, z)] = 300.0;
            }
        }
    }

    let th =
        diface_rs::geometric::choose_threshold(&volume, &diface_rs::geometric::Threshold::Auto);
    let best =
        diface_rs::geometric::head_statistics_best(&volume, th, 160).expect("segmented stats");
    // Superior axis should be close to patient Z (not pulled into the table).
    assert!(
        best.superior[2].abs() > 0.8,
        "superior should be near +Z, got {:?}",
        best.superior
    );
}

#[test]
fn segmentation_margin_controls_removal_amount() {
    // Smaller brain margin cuts closer to the brain (removes more face); larger
    // margin is safer (removes less). This is the control the viewer's Depth and
    // Preset sliders map to.
    let volume = head_with_table();
    let tight = SegmentationBackend::new(SegBackendParams {
        brain_margin_mm: 0.0,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    let loose = SegmentationBackend::new(SegBackendParams {
        brain_margin_mm: 30.0,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    assert!(
        tight.count_removed() >= loose.count_removed(),
        "smaller margin should remove at least as much ({} vs {})",
        tight.count_removed(),
        loose.count_removed()
    );
    // Deep brain is still preserved even with margin 0.
    let brain = world_to_index(&volume, 0.0, 0.0, 0.0);
    assert!(!tight.is_removed(brain.0, brain.1, brain.2));
}

#[test]
fn brain_protect_band_shrinks_removal_and_spares_the_core() {
    // `brain_protect_mm` is the hard safety band kept around the intracranial
    // core (`cavity ∪ vault`). A wider band must remove strictly less face while
    // still sparing the brain. This is the control exposed as `--brain-protect`.
    let volume = head_with_table();
    let mk = |protect: f64| {
        SegmentationBackend::new(SegBackendParams {
            brain_margin_mm: 20.0,
            brain_protect_mm: protect,
            ..SegBackendParams::default()
        })
        .compute_mask(&volume)
        .expect("mask")
    };
    let none = mk(0.0);
    let wide = mk(15.0);
    assert!(
        wide.count_removed() <= none.count_removed(),
        "a wider safety band must not remove more ({} vs {})",
        wide.count_removed(),
        none.count_removed()
    );
    assert!(
        wide.count_removed() < none.count_removed(),
        "a wider safety band should remove strictly less on the phantom"
    );

    // Neither mask may remove a brain voxel.
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    for mask in [&none, &wide] {
        let mut brain_removed = 0usize;
        for z in 0..volume.nz() {
            for y in 0..volume.ny() {
                for x in 0..volume.nx() {
                    let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                    let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                    let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                    if seg.brain_at(cx, cy, cz) && mask.is_removed(x, y, z) {
                        brain_removed += 1;
                    }
                }
            }
        }
        assert_eq!(brain_removed, 0, "the safety band must keep all brain voxels");
    }
}

#[test]
fn deflesh_removes_external_tissue_and_keeps_brain() {
    use diface_rs::MaskRegion;
    let volume = head_with_table();
    let backend = SegmentationBackend::new(SegBackendParams {
        region: MaskRegion::ExternalSoftTissue,
        brain_margin_mm: 10.0,
        ..SegBackendParams::default()
    });
    let mask = backend.compute_mask(&volume).expect("deflesh mask");

    // Deep brain preserved.
    let brain = world_to_index(&volume, 0.0, 0.0, 0.0);
    assert!(!mask.is_removed(brain.0, brain.1, brain.2), "brain spared");

    // Nose (anterior external) removed.
    let nose = world_to_index(&volume, 0.0, -70.0, 0.0);
    assert!(mask.is_removed(nose.0, nose.1, nose.2), "nose removed");

    // Table (posterior external) is **kept** by default: the posterior gate
    // preserves the neck/back of the head, stripping only the anterior face.
    let table = world_to_index(&volume, 0.0, 75.0, -70.0);
    assert!(!mask.is_removed(table.0, table.1, table.2), "posterior table kept");

    // With the gate disabled (`INFINITY`), the posterior table *is* removed.
    let all = SegmentationBackend::new(SegBackendParams {
        region: MaskRegion::ExternalSoftTissue,
        deflesh_posterior_mm: f64::INFINITY,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .expect("deflesh mask (all)");
    assert!(all.is_removed(table.0, table.1, table.2), "table removed when gate off");
    assert!(
        all.count_removed() > mask.count_removed(),
        "disabling the posterior gate removes more"
    );

    // Deflesh removes more than the flat face cut.
    let face = SegmentationBackend::new(SegBackendParams {
        region: MaskRegion::Face,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .unwrap();
    assert!(
        mask.count_removed() > face.count_removed(),
        "deflesh ({}) should remove more than face cut ({})",
        mask.count_removed(),
        face.count_removed()
    );

    // The reported regression: deflesh must not clip the frontal lobes / anterior
    // temporal poles — i.e. it must not remove any voxel inside the segmented
    // brain. It may only remove *external* tissue (outside the skull vault).
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    let mut brain_removed = 0usize;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                if seg.brain_at(cx, cy, cz) && mask.is_removed(x, y, z) {
                    brain_removed += 1;
                }
            }
        }
    }
    assert_eq!(brain_removed, 0, "deflesh must not remove any brain voxel");

    // The vault is the bony shell enclosing the cavity; deflesh must not remove
    // any of it either (it is the keep boundary).
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    assert!(
        seg.vault.iter().any(|&b| b != 0),
        "CT phantom should yield a vault"
    );
    let mut vault_removed = 0usize;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                if seg.vault_at(cx, cy, cz) && mask.is_removed(x, y, z) {
                    vault_removed += 1;
                }
            }
        }
    }
    assert_eq!(vault_removed, 0, "deflesh must not remove any vault voxel");
}

#[test]
fn vault_is_a_shell_that_excludes_the_face() {
    // The cranial vault should be a strict subset of the skull and should not
    // reach as far anteriorly as the facial skeleton/nose.
    let volume = head_with_table();
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    let skull_n = seg.skull.iter().filter(|&&b| b != 0).count();
    let vault_n = seg.vault.iter().filter(|&&b| b != 0).count();
    assert!(vault_n > 0, "vault detected");
    assert!(
        vault_n < skull_n,
        "vault ({vault_n}) must be a strict subset of skull ({skull_n})"
    );
    // The vault's anterior-most point must be posterior to the skull's anterior
    // face (which includes the nose at the phantom's -Y).
    let skull_ant = seg.skull_anterior(&volume).expect("skull anterior");
    let vault_ant = seg.vault_anterior(&volume).expect("vault anterior");
    assert!(
        vault_ant[1] > skull_ant[1] + 1.0,
        "vault front ({:.0}) should be posterior to the face ({:.0})",
        vault_ant[1],
        skull_ant[1]
    );
}

#[test]
fn segmentation_rejects_empty_volume() {
    let volume = Volume {
        data: Vec::new(),
        dims: [0, 0, 0],
        spacing: [1.0, 1.0, 1.0],
        origin: [0.0, 0.0, 0.0],
        dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        rescale: Vec::new(),
        modality: "CT".to_string(),
    };
    assert!(segment(&volume, &SegParams::default()).is_err());
}
