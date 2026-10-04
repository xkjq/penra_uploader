//! Debug the in-house head/brain segmentation for a series.
//!
//! ```bash
//! cargo run --example seg_debug -- <series_dir>
//! ```

use std::path::Path;

use diface_rs::segmentation::{segment, SegParams};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: seg_debug <series_dir> [--ascii] [--protect mm]");
    let ascii = std::env::args().any(|a| a == "--ascii");
    let protect_mm: f64 = {
        let args: Vec<String> = std::env::args().collect();
        args.iter()
            .position(|a| a == "--protect")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0)
    };
    let files = diface_rs::series::collect_dicom_files(Path::new(&dir), true).expect("list");
    let (volume, _entries) = diface_rs::series::load_series(&files).expect("load");
    println!(
        "volume dims {:?} spacing {:?} modality {}",
        volume.dims, volume.spacing, volume.modality
    );

    let seg = segment(&volume, &SegParams::default()).expect("segment");
    println!("coarse dims {:?} stride {:?}", seg.dims, seg.stride);
    println!(
        "head cells {} brain cells {}",
        seg.head_count(),
        seg.brain_count()
    );
    println!("head centroid {:?}", seg.head_centroid);
    println!("brain centroid {:?}", seg.brain_centroid);
    // Coarse index -> source voxel coordinate (mirrors the private helper).
    let c2v = |i: usize, cd: [usize; 3], st: [usize; 3]| -> (usize, usize, usize) {
        let cz = i / (cd[0] * cd[1]);
        let rem = i % (cd[0] * cd[1]);
        let cy = rem / cd[0];
        let cx = rem % cd[0];
        (
            (cx * st[0]).min(volume.dims[0] - 1),
            (cy * st[1]).min(volume.dims[1] - 1),
            (cz * st[2]).min(volume.dims[2] - 1),
        )
    };
    let mut brain_front = f64::MAX;
    for z in 0..seg.dims[2] {
        for y in 0..seg.dims[1] {
            for x in 0..seg.dims[0] {
                if !seg.brain_at(x, y, z) {
                    continue;
                }
                let (vx, vy, vz) = c2v(seg.cidx(x, y, z), seg.dims, seg.stride);
                brain_front = brain_front.min(volume.world(vx, vy, vz)[1]);
            }
        }
    }
    println!("brain anterior (min Y) = {brain_front:.0}");
    for (label, grid) in [("cavity", &seg.cavity)] {
        let mut front = f64::MAX;
        for z in 0..seg.dims[2] {
            for y in 0..seg.dims[1] {
                for x in 0..seg.dims[0] {
                    if grid[seg.cidx(x, y, z)] == 0 {
                        continue;
                    }
                    let (vx, vy, vz) = c2v(seg.cidx(x, y, z), seg.dims, seg.stride);
                    front = front.min(volume.world(vx, vy, vz)[1]);
                }
            }
        }
        println!("{label} anterior (min Y) = {front:.0}");
    }

    // Compare the head frame with and without segmentation.
    let th =
        diface_rs::geometric::choose_threshold(&volume, &diface_rs::geometric::Threshold::Auto);
    if let Some(s) = diface_rs::geometric::head_statistics(&volume, th, 160) {
        println!("plain stats:   superior {:?} half {:?}", s.superior, s.half);
    }
    if let Some(s) = diface_rs::geometric::head_statistics_best(&volume, th, 160) {
        println!("segmented:     superior {:?} half {:?}", s.superior, s.half);
    }

    // Physical extents of the head component.
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                let p = volume.world(x, y, z);
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                if seg.head_at(cx, cy, cz) {
                    for a in 0..3 {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
            }
        }
    }
    println!(
        "head extent XYZ = [{:.0}, {:.0}, {:.0}] mm",
        hi[0] - lo[0],
        hi[1] - lo[1],
        hi[2] - lo[2]
    );

    // Skull / vault statistics and anteriormost points.
    let skull_cells = seg.skull.iter().filter(|&&b| b != 0).count();
    let vault_cells = seg.vault.iter().filter(|&&b| b != 0).count();
    println!(
        "skull cells {skull_cells} vault cells {vault_cells} (coarse {}x{}x{})",
        seg.dims[0], seg.dims[1], seg.dims[2]
    );
    if let Some(p) = seg.skull_anterior(&volume) {
        println!(
            "skull anterior  (min Y) = [{:.0}, {:.0}, {:.0}]",
            p[0], p[1], p[2]
        );
    }
    if let Some(p) = seg.vault_anterior(&volume) {
        println!(
            "vault anterior  (min Y) = [{:.0}, {:.0}, {:.0}]",
            p[0], p[1], p[2]
        );
    }

    // How much brain does each mask remove? A safe deface must remove ~0 brain.

    // ASCII overview: a mid-sagittal slice (patient X ~ brain centroid X) of the
    // deflesh result, showing brain (b), cavity (c), vault (#), removed (.),
    // kept-tissue (o) and air (space). Useful to eyeball the mask boundary.
    use diface_rs::backend::DefacingBackend;
    let deflesh = diface_rs::SegmentationBackend::new(diface_rs::SegBackendParams {
        region: diface_rs::MaskRegion::ExternalSoftTissue,
        ..diface_rs::SegBackendParams::default()
    })
    .compute_mask(&volume)
    .expect("deflesh mask");
    let mid_x = {
        // Coarse X nearest the brain centroid.
        let c = seg.brain_centroid;
        let mut best = 0usize;
        let mut bd = f64::MAX;
        for x in 0..volume.nx() {
            let p = volume.world(x, 0, 0)[0];
            let d = (p - c[0]).abs();
            if d < bd {
                bd = d;
                best = x;
            }
        }
        best
    };
    println!("--- mid-sagittal slice (voxel X = {mid_x}), rows=Z(inf->sup) cols=Y(post->ant) ---");
    if ascii {
        for z in (0..volume.nz()).rev() {
            let mut line = String::new();
            for y in 0..volume.ny() {
                let x = mid_x;
                let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                let ch = if seg.brain_at(cx, cy, cz) {
                    'b'
                } else if seg.cavity_at(cx, cy, cz) {
                    'c'
                } else if seg.vault_at(cx, cy, cz) {
                    '#'
                } else if deflesh.is_removed(x, y, z) {
                    '.'
                } else if volume.value(x, y, z) > th {
                    'o'
                } else {
                    ' '
                };
                line.push(ch);
            }
            println!("{line}");
        }
    }

    // How much brain does each mask remove? A safe deface must remove ~0 brain.
    for (label, region) in [
        ("face", diface_rs::MaskRegion::Face),
        ("deflesh", diface_rs::MaskRegion::ExternalSoftTissue),
    ] {
        let backend = diface_rs::SegmentationBackend::new(diface_rs::SegBackendParams {
            region,
            brain_protect_mm: protect_mm,
            ..diface_rs::SegBackendParams::default()
        });
        let mask = backend.compute_mask(&volume).expect("mask");
        let mut brain_removed = 0usize;
        let mut brain_total = 0usize;
        // Split removed voxels by patient +Y relative to the brain centroid, and
        // count external (outside the segmented head) removed voxels. (The +Y
        // sign is the volume's patient axis; whether it is anterior depends on
        // the series orientation, so these are labelled by sign.)
        let cy_split = seg.brain_centroid[1];
        let (mut ant, mut post, mut outside_head, mut intracranial_removed) =
            (0usize, 0usize, 0usize, 0usize);
        for z in 0..volume.nz() {
            for y in 0..volume.ny() {
                for x in 0..volume.nx() {
                    let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                    let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                    let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                    if seg.brain_at(cx, cy, cz) {
                        brain_total += 1;
                        if mask.is_removed(x, y, z) {
                            brain_removed += 1;
                        }
                    }
                    // Anything removed inside the skull interior (cavity or vault)
                    // means the cut reached past the outer table.
                    if mask.is_removed(x, y, z)
                        && (seg.cavity_at(cx, cy, cz) || seg.vault_at(cx, cy, cz))
                    {
                        intracranial_removed += 1;
                    }
                    if mask.is_removed(x, y, z) {
                        if volume.world(x, y, z)[1] >= cy_split {
                            ant += 1;
                        } else {
                            post += 1;
                        }
                        if !seg.head_at(cx, cy, cz) {
                            outside_head += 1;
                        }
                    }
                }
            }
        }
        println!(
            "{label:>8}: removed {} voxels ({:.1}% of volume); brain removed {} / {} ({:.2}%); \
             intracranial removed {}; +Y {:.0}% / -Y {:.0}%; outside head {:.0}%",
            mask.count_removed(),
            100.0 * mask.count_removed() as f64 / volume.len() as f64,
            brain_removed,
            brain_total,
            100.0 * brain_removed as f64 / brain_total.max(1) as f64,
            intracranial_removed,
            100.0 * ant as f64 / mask.count_removed().max(1) as f64,
            100.0 * post as f64 / mask.count_removed().max(1) as f64,
            100.0 * outside_head as f64 / mask.count_removed().max(1) as f64,
        );
        // Vault cells must survive deflesh (the vault is the keep boundary).
        if region == diface_rs::MaskRegion::ExternalSoftTissue {
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
            println!("          vault voxels removed by deflesh: {vault_removed}");
        }
    }
}
