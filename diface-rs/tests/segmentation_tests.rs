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
        // This test is about the anterior face cut, not the vertex strip.
        vertex_strip: false,
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
    // `brain_protect_mm` is the physical safety band kept around the intracranial
    // core. Deflesh is the mode where the band *is* the keep boundary, so a wider
    // band must remove strictly less. This is the control exposed as
    // `--brain-protect`.
    let volume = head_with_table();
    let mk = |protect: f64| {
        SegmentationBackend::new(SegBackendParams {
            region: diface_rs::MaskRegion::ExternalSoftTissue,
            deflesh_posterior_mm: f64::INFINITY,
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
        assert_eq!(
            brain_removed, 0,
            "the safety band must keep all brain voxels"
        );
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
    assert!(
        !mask.is_removed(table.0, table.1, table.2),
        "posterior table kept"
    );

    // With the gate disabled (`INFINITY`), the posterior table *is* removed.
    let all = SegmentationBackend::new(SegBackendParams {
        region: MaskRegion::ExternalSoftTissue,
        deflesh_posterior_mm: f64::INFINITY,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .expect("deflesh mask (all)");
    assert!(
        all.is_removed(table.0, table.1, table.2),
        "table removed when gate off"
    );
    assert!(
        all.count_removed() > mask.count_removed(),
        "disabling the posterior gate removes more"
    );

    // Deflesh removes more than the flat face cut.
    let face = SegmentationBackend::new(SegBackendParams {
        region: MaskRegion::Face,
        // Compare the classic face cut; the optional vertex strip can remove
        // more than deflesh.
        vertex_strip: false,
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
fn deflesh_smooth_feathers_the_boundary_without_changing_removal() {
    use diface_rs::MaskRegion;
    let volume = head_with_table();
    let mk = |smooth: f64| {
        SegmentationBackend::new(SegBackendParams {
            region: MaskRegion::ExternalSoftTissue,
            brain_margin_mm: 10.0,
            deflesh_smooth_mm: smooth,
            ..SegBackendParams::default()
        })
        .compute_mask(&volume)
        .expect("deflesh mask")
    };

    let hard = mk(0.0);
    let soft = mk(6.0);

    // The binary removal set is identical: feathering only softens the edge, so
    // the safety guarantee (0 brain / 0 vault removed) still holds.
    assert_eq!(
        hard.count_removed(),
        soft.count_removed(),
        "feathering must not change which voxels are removed"
    );
    // The hard mask has no feather buffer; the soft one does.
    assert!(hard.weight.is_empty());
    assert_eq!(soft.weight.len(), soft.remove.len());

    // At least one removed voxel is partially blended (0 < w < 1)...
    let partial = soft
        .weight
        .iter()
        .enumerate()
        .filter(|(i, w)| soft.remove[*i] != 0 && **w > 0.0 && **w < 1.0)
        .count();
    assert!(partial > 0, "expected feathered (partial) voxels");
    // ...and the boundary itself (weight 0) exists.
    let zero = soft
        .weight
        .iter()
        .enumerate()
        .filter(|(i, w)| soft.remove[*i] != 0 && **w == 0.0)
        .count();
    assert!(zero > 0, "expected boundary voxels at weight 0");

    // Deeper into the removed region the weight approaches 1.
    assert!(
        soft.weight.iter().cloned().fold(0.0f32, f32::max) >= 0.999,
        "some removed voxels should be fully removed"
    );

    // Feathering does not add brain removal.
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    let mut brain_removed = 0usize;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                if seg.brain_at(cx, cy, cz) && soft.is_removed(x, y, z) {
                    brain_removed += 1;
                }
            }
        }
    }
    assert_eq!(brain_removed, 0, "feathered deflesh must keep all brain");
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

/// A head with a thin skull surrounding the brain and two soft-tissue globes in
/// the orbits (anterior-inferior to the frontal lobes), so the optional orbit
/// removal can be exercised.
fn head_with_orbits() -> Volume {
    let (nx, ny, nz) = (96usize, 96usize, 96usize);
    let spacing = 2.0f64;
    let c = (nx as f64 - 1.0) / 2.0;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let mut data = vec![-1000.0f32; nx * ny * nz];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let px = (x as f64 - c) * spacing;
                let py = (y as f64 - c) * spacing;
                let pz = (z as f64 - c) * spacing;
                // Head/face soft tissue (outer ellipsoid).
                let head = (px / 60.0).powi(2) + (py / 78.0).powi(2) + (pz / 80.0).powi(2);
                if head <= 1.0 {
                    data[idx(x, y, z)] = 40.0;
                }
                // Brain interior and a ~7 mm bone shell around it.
                let brain = (px / 48.0).powi(2) + (py / 52.0).powi(2) + (pz / 57.0).powi(2);
                if brain <= 0.92 {
                    data[idx(x, y, z)] = 40.0;
                } else if brain <= 1.20 {
                    data[idx(x, y, z)] = 900.0;
                }
                // Globes: soft-tissue spheres in the orbits, just outside the
                // skull and below the frontal lobes. They are painted only where
                // the bone shell is absent, so the skull stays intact.
                for eye_x in [-18.0f64, 18.0] {
                    let globe = ((px - eye_x) / 9.0).powi(2)
                        + ((py + 58.0) / 9.0).powi(2)
                        + ((pz + 20.0) / 9.0).powi(2);
                    if head <= 1.0 && brain > 1.2 && globe <= 1.0 {
                        data[idx(x, y, z)] = 40.0;
                    }
                }
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

#[test]
fn include_orbits_removes_the_globes_and_spares_the_brain() {
    let volume = head_with_orbits();
    // Patient coordinates of both globe centres (see `head_with_orbits`).
    let right = world_to_index(&volume, 18.0, -58.0, -20.0);
    let left = world_to_index(&volume, -18.0, -58.0, -20.0);

    // A conservative margin puts the flat cut well in front of the brain, so the
    // globes are left behind by the plain face cut. A generous safety band would
    // otherwise protect them, so the orbit extension guards on the brain only.
    let base = SegBackendParams {
        region: diface_rs::MaskRegion::Face,
        brain_margin_mm: 30.0,
        brain_protect_mm: 14.0,
        // Scope the posterior-limit assertion to the orbit-only extension.
        vertex_strip: false,
        ..SegBackendParams::default()
    };
    let without = SegmentationBackend::new(base.clone())
        .compute_mask(&volume)
        .expect("mask without orbits");
    let with = SegmentationBackend::new(SegBackendParams {
        include_orbits: true,
        ..base
    })
    .compute_mask(&volume)
    .expect("mask with orbits");

    assert!(
        !without.is_removed(right.0, right.1, right.2),
        "the flat cut should leave the right globe behind"
    );
    assert!(
        with.is_removed(right.0, right.1, right.2),
        "include_orbits should remove the right globe"
    );
    assert!(
        with.is_removed(left.0, left.1, left.2),
        "include_orbits should remove the left globe"
    );
    assert!(
        with.count_removed() > without.count_removed(),
        "the orbit extension should remove more voxels ({} vs {})",
        with.count_removed(),
        without.count_removed()
    );

    // The globes really do fall inside the padded safety band, which is exactly
    // why the orbit region is guarded by the segmented brain/vault instead.
    let padded = diface_rs::segmentation::brain_protect_mask(
        &volume,
        &SegParams::default(),
        base.brain_protect_mm,
    )
    .expect("protected core")
    .expect("brain found");
    assert!(
        padded.is_removed(right.0, right.1, right.2),
        "test setup: the right globe should be inside the padded core"
    );

    // Safety: the brain is never touched by either mask (the orbit region is
    // additionally guarded by the segmented brain/vault, not the cavity).
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    for (label, mask) in [("without", &without), ("with", &with)] {
        let mut brain_removed = 0usize;
        for z in 0..volume.nz() {
            for y in 0..volume.ny() {
                for x in 0..volume.nx() {
                    if seg.brain_at(
                        (x / seg.stride[0]).min(seg.dims[0] - 1),
                        (y / seg.stride[1]).min(seg.dims[1] - 1),
                        (z / seg.stride[2]).min(seg.dims[2] - 1),
                    ) && mask.is_removed(x, y, z)
                    {
                        brain_removed += 1;
                    }
                }
            }
        }
        assert_eq!(brain_removed, 0, "{label} orbits must not remove brain");
    }

    // The orbit region must stay in front of the brain: nothing may be removed
    // behind the brain's anterior pole (which would be the inferior frontal
    // lobes / temporal fossa).
    let mut front = f64::MAX;
    for cz in 0..seg.dims[2] {
        for cy in 0..seg.dims[1] {
            for cx in 0..seg.dims[0] {
                if !seg.brain_at(cx, cy, cz) {
                    continue;
                }
                let x = (cx * seg.stride[0]).min(volume.nx() - 1);
                let y = (cy * seg.stride[1]).min(volume.ny() - 1);
                let z = (cz * seg.stride[2]).min(volume.nz() - 1);
                front = front.min(volume.world(x, y, z)[1]);
            }
        }
    }
    let mut posterior_most = f64::MIN;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if with.is_removed(x, y, z) {
                    posterior_most = posterior_most.max(volume.world(x, y, z)[1]);
                }
            }
        }
    }
    assert!(
        posterior_most <= front,
        "orbit removal reaches {} mm behind the brain front",
        posterior_most - front
    );
}

#[test]
fn include_zygoma_removes_the_cheekbone_and_keeps_the_brain() {
    let volume = head_with_orbits();
    // A lateral, sub-orbital point behind the cut plane, on the cheek where the
    // zygomatic arch would be and clear of the brain's safety band.
    let cheek = world_to_index(&volume, 50.0, 0.0, -35.0);

    let base = SegBackendParams {
        region: diface_rs::MaskRegion::Face,
        // Conservative margin so the plane cut stops well in front of the cheek.
        brain_margin_mm: 30.0,
        brain_protect_mm: 2.0,
        vertex_strip: false,
        ..SegBackendParams::default()
    };
    let without = SegmentationBackend::new(base.clone())
        .compute_mask(&volume)
        .expect("mask without zygoma");
    let with = SegmentationBackend::new(SegBackendParams {
        include_zygoma: true,
        ..base
    })
    .compute_mask(&volume)
    .expect("mask with zygoma");

    assert!(
        !without.is_removed(cheek.0, cheek.1, cheek.2),
        "the plain face cut leaves the lateral cheek"
    );
    assert!(
        with.is_removed(cheek.0, cheek.1, cheek.2),
        "include_zygoma should remove the cheek"
    );
    assert!(
        with.count_removed() > without.count_removed(),
        "the zygoma extension should remove more voxels ({} vs {})",
        with.count_removed(),
        without.count_removed()
    );

    // The brain and CSF are protected: no segmented brain voxel may be removed.
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                assert!(
                    !(seg.brain_at(cx, cy, cz) && with.is_removed(x, y, z)),
                    "the zygoma extension must not remove brain voxels"
                );
            }
        }
    }
}

#[test]
fn include_ears_removes_lateral_ear_tissue_and_keeps_the_occiput() {
    let volume = head_with_orbits();
    // A lateral soft-tissue voxel at ear height, just outside the vault.
    let ear = world_to_index(&volume, 56.0, 0.0, 0.0);
    // A posterior scalp voxel on the occiput.
    let occiput = world_to_index(&volume, 0.0, 70.0, 0.0);

    let base = SegBackendParams {
        region: diface_rs::MaskRegion::Face,
        brain_margin_mm: 30.0,
        ..SegBackendParams::default()
    };
    let without = SegmentationBackend::new(base.clone())
        .compute_mask(&volume)
        .expect("mask without ears");
    let with = SegmentationBackend::new(SegBackendParams {
        include_ears: true,
        ..base
    })
    .compute_mask(&volume)
    .expect("mask with ears");

    assert!(
        !without.is_removed(ear.0, ear.1, ear.2),
        "the flat cut should leave the ear-level tissue"
    );
    assert!(
        with.is_removed(ear.0, ear.1, ear.2),
        "include_ears should remove the ear-level tissue"
    );
    assert!(
        !with.is_removed(occiput.0, occiput.1, occiput.2),
        "the occiput should be preserved"
    );
    assert!(
        with.count_removed() > without.count_removed(),
        "the ear extension should remove more voxels ({} vs {})",
        with.count_removed(),
        without.count_removed()
    );
}

#[test]
fn removal_regions_are_visually_continuous() {
    // The face, orbit and ear regions must form one 26-connected removal area;
    // isolated specks are pruned by the backend.
    let volume = head_with_orbits();
    let mask = SegmentationBackend::new(SegBackendParams {
        region: diface_rs::MaskRegion::Face,
        include_orbits: true,
        include_ears: true,
        include_zygoma: true,
        brain_margin_mm: 10.0,
        ..SegBackendParams::default()
    })
    .compute_mask(&volume)
    .expect("mask");
    assert!(mask.count_removed() > 0);

    let [nx, ny, nz] = mask.dims;
    let mut seen = vec![0u8; nx * ny * nz];
    let mut components = 0usize;
    for start in 0..seen.len() {
        if mask.remove[start] == 0 || seen[start] != 0 {
            continue;
        }
        components += 1;
        let mut stack = vec![start];
        seen[start] = 1;
        while let Some(i) = stack.pop() {
            let z = i / (nx * ny);
            let r = i % (nx * ny);
            let y = r / nx;
            let x = r % nx;
            for dz in -1isize..=1 {
                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        if dx == 0 && dy == 0 && dz == 0 {
                            continue;
                        }
                        let (xx, yy, zz) = (x as isize + dx, y as isize + dy, z as isize + dz);
                        if xx < 0 || yy < 0 || zz < 0 {
                            continue;
                        }
                        let (xx, yy, zz) = (xx as usize, yy as usize, zz as usize);
                        if xx >= nx || yy >= ny || zz >= nz {
                            continue;
                        }
                        let j = (zz * ny + yy) * nx + xx;
                        if mask.remove[j] != 0 && seen[j] == 0 {
                            seen[j] = 1;
                            stack.push(j);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(components, 1, "removal must be one continuous region");
}

#[test]
fn vertex_strip_removes_superior_vault_and_spares_brain() {
    let volume = head_with_orbits();
    // A superior vault point above the ear-top level.
    let vertex = world_to_index(&volume, 0.0, 0.0, 62.0);
    let base = SegBackendParams {
        region: diface_rs::MaskRegion::Face,
        ..SegBackendParams::default()
    };
    let with = SegmentationBackend::new(base.clone())
        .compute_mask(&volume)
        .expect("mask with vertex strip");
    let without = SegmentationBackend::new(SegBackendParams {
        vertex_strip: false,
        ..base
    })
    .compute_mask(&volume)
    .expect("mask without vertex strip");

    assert!(
        !without.is_removed(vertex.0, vertex.1, vertex.2),
        "the plain face cut should leave the vertex vault"
    );
    assert!(
        with.is_removed(vertex.0, vertex.1, vertex.2),
        "vertex_strip should remove the superior vault"
    );
    assert!(
        with.count_removed() > without.count_removed(),
        "the vertex strip should remove more ({} vs {})",
        with.count_removed(),
        without.count_removed()
    );

    // The brain remains intact.
    let seg = segment(&volume, &SegParams::default()).expect("segment");
    let mut brain_removed = 0usize;
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                if seg.brain_at(cx, cy, cz) && with.is_removed(x, y, z) {
                    brain_removed += 1;
                }
            }
        }
    }
    assert_eq!(brain_removed, 0, "the vertex strip must keep all brain");
}
