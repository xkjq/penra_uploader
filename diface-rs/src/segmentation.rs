//! In-house head and brain segmentation.
//!
//! A dependency-free pipeline that
//! 1. thresholds the volume,
//! 2. isolates the **head** as the largest compact connected component near the
//!    volume centre (discarding the patient table, which forms a long, thin
//!    component that often touches the head),
//! 3. estimates the **brain** region within the head:
//!    - CT: soft-tissue region growing from the head centre, bounded by bone and
//!      air (an approximate intracranial mask),
//!    - MR: region growing from the centre within a brain-intensity band.
//!
//! Both masks are then cleaned with morphological open/close and a final
//! largest-connected-component pass. The result is deliberately coarse but
//! robust, and drives the defacing geometry (front-of-brain cut) without any
//! external model or atlas.

use std::collections::VecDeque;

use crate::geometry::{add, dot, normalize, scale, sub, V3};
use crate::volume::{Mask, Volume};

/// Segmentation parameters.
#[derive(Clone, Debug)]
pub struct SegParams {
    /// Foreground threshold selection (reuses the geometric `Threshold`).
    pub threshold: crate::geometric::Threshold,
    /// Working resolution for the coarse morphological steps.
    pub coarse_target: usize,
    /// Radius (mm) of the central ball used to seed the head component.
    pub head_seed_radius_mm: f64,
    /// Minimum head volume as a fraction of the whole volume.
    pub min_head_fraction: f64,
    /// Maximum head volume as a fraction of the whole volume.
    pub max_head_fraction: f64,
    /// Morphological closing radius (voxels) applied to fill holes.
    pub close_radius: u32,
    /// Morphological opening radius (voxels) applied to remove specks.
    pub open_radius: u32,
    /// Keep only head-component voxels within this radius (mm) of the head's
    /// superior centroid. Prevents leakage into the neck/body.
    pub head_radius_mm: f64,
    /// Brain-intensity band, as a fraction of the head's intensity range, used
    /// for region growing from the head centre on non-CT modalities.
    pub mr_brain_band: (f64, f64),
    /// CT bone threshold (HU). Voxels above this are treated as skull and used
    /// as the brain grow barrier on CT.
    pub bone_hu: f32,
    /// Morphological closing radius (voxels) applied to the detected skull to
    /// seal small gaps before it is used as a barrier.
    pub skull_close_radius: u32,
    /// Shell thickness (coarse voxels) used to select the **cranial vault** from
    /// the skull as the bone surrounding the intracranial cavity.
    pub vault_shell_radius: u32,
}

impl Default for SegParams {
    fn default() -> Self {
        SegParams {
            threshold: crate::geometric::Threshold::Auto,
            coarse_target: 160,
            head_seed_radius_mm: 25.0,
            min_head_fraction: 0.002,
            max_head_fraction: 0.9,
            close_radius: 2,
            open_radius: 1,
            head_radius_mm: 115.0,
            mr_brain_band: (0.25, 0.75),
            bone_hu: 250.0,
            skull_close_radius: 2,
            vault_shell_radius: 2,
        }
    }
}

/// The segmentation result, in a coarse grid plus a mapping back to voxels.
#[derive(Clone, Debug)]
pub struct Segmentation {
    /// Coarse grid dims `[x, y, z]`.
    pub dims: [usize; 3],
    /// Coarse voxel stride per axis (voxels of the source volume).
    pub stride: [usize; 3],
    /// Head occupancy (1 = head).
    pub head: Vec<u8>,
    /// Brain occupancy (1 = brain); subset of `head`.
    pub brain: Vec<u8>,
    /// Skull occupancy (1 = skull), coarse; empty on non-CT modalities.
    pub skull: Vec<u8>,
    /// Cranial-vault occupancy (the bony shell enclosing the intracranial
    /// cavity), coarse; empty on non-CT.
    pub vault: Vec<u8>,
    /// Intracranial cavity (brain + CSF, bounded by the inner table), coarse;
    /// empty on non-CT.
    pub cavity: Vec<u8>,
    /// Source-volume voxel coordinate of coarse voxel `(0,0,0)` origin offset.
    pub origin_voxel: [usize; 3],
    /// Head centroid in patient coordinates.
    pub head_centroid: V3,
    /// Brain centroid in patient coordinates.
    pub brain_centroid: V3,
}

impl Segmentation {
    #[inline]
    pub fn cidx(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.dims[1] + y) * self.dims[0] + x
    }

    pub fn head_at(&self, x: usize, y: usize, z: usize) -> bool {
        x < self.dims[0]
            && y < self.dims[1]
            && z < self.dims[2]
            && self.head[self.cidx(x, y, z)] != 0
    }

    pub fn brain_at(&self, x: usize, y: usize, z: usize) -> bool {
        x < self.dims[0]
            && y < self.dims[1]
            && z < self.dims[2]
            && self.brain[self.cidx(x, y, z)] != 0
    }

    pub fn skull_at(&self, x: usize, y: usize, z: usize) -> bool {
        !self.skull.is_empty()
            && x < self.dims[0]
            && y < self.dims[1]
            && z < self.dims[2]
            && self.skull[self.cidx(x, y, z)] != 0
    }

    pub fn vault_at(&self, x: usize, y: usize, z: usize) -> bool {
        !self.vault.is_empty()
            && x < self.dims[0]
            && y < self.dims[1]
            && z < self.dims[2]
            && self.vault[self.cidx(x, y, z)] != 0
    }

    pub fn cavity_at(&self, x: usize, y: usize, z: usize) -> bool {
        !self.cavity.is_empty()
            && x < self.dims[0]
            && y < self.dims[1]
            && z < self.dims[2]
            && self.cavity[self.cidx(x, y, z)] != 0
    }

    /// Most anterior (minimum patient Y) point of the cranial vault, if present.
    pub fn vault_anterior(&self, volume: &Volume) -> Option<V3> {
        self.anterior_of(&self.vault, volume)
    }

    /// Most anterior (minimum patient Y) point of a coarse grid.
    fn anterior_of(&self, grid: &[u8], volume: &Volume) -> Option<V3> {
        if grid.is_empty() {
            return None;
        }
        let mut best: Option<(f64, V3)> = None;
        for (i, &v) in grid.iter().enumerate() {
            if v == 0 {
                continue;
            }
            let (x, y, z) = coarse_to_voxel(i, self.dims, self.stride, volume.dims);
            let p = volume.world(x, y, z);
            if best.as_ref().map(|(y0, _)| p[1] < *y0).unwrap_or(true) {
                best = Some((p[1], p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// Most anterior (minimum patient Y) point of the skull, if present.
    pub fn skull_anterior(&self, volume: &Volume) -> Option<V3> {
        if self.skull.is_empty() {
            return None;
        }
        let mut best: Option<(f64, V3)> = None;
        for (i, &v) in self.skull.iter().enumerate() {
            if v == 0 {
                continue;
            }
            let (x, y, z) = coarse_to_voxel(i, self.dims, self.stride, volume.dims);
            let p = volume.world(x, y, z);
            if best.as_ref().map(|(y0, _)| p[1] < *y0).unwrap_or(true) {
                best = Some((p[1], p));
            }
        }
        best.map(|(_, p)| p)
    }

    pub fn head_count(&self) -> usize {
        self.head.iter().filter(|&&v| v != 0).count()
    }

    pub fn brain_count(&self) -> usize {
        self.brain.iter().filter(|&&v| v != 0).count()
    }
}

/// Segment a volume into head and brain.
pub fn segment(volume: &Volume, params: &SegParams) -> Result<Segmentation, String> {
    if volume.is_empty() {
        return Err("cannot segment an empty volume".to_string());
    }
    let dims = volume.dims;
    // Per-axis stride so anisotropic volumes (thin CT slices) are not
    // over-decimated along Z. Target a roughly cubic coarse cell.
    let target = params.coarse_target.max(8) as f64;
    let max_extent = (0..3)
        .map(|a| dims[a] as f64 * volume.spacing[a])
        .fold(0.0f64, f64::max);
    let cell_mm = (max_extent / target).max(1e-3);
    let stride = [
        ((cell_mm / volume.spacing[0].max(1e-3)).round() as usize).max(1),
        ((cell_mm / volume.spacing[1].max(1e-3)).round() as usize).max(1),
        ((cell_mm / volume.spacing[2].max(1e-3)).round() as usize).max(1),
    ];
    let cd = [
        (dims[0] + stride[0] - 1) / stride[0],
        (dims[1] + stride[1] - 1) / stride[1],
        (dims[2] + stride[2] - 1) / stride[2],
    ];
    let cn = cd[0] * cd[1] * cd[2];

    let threshold = crate::geometric::choose_threshold(volume, &params.threshold);

    // Coarse foreground occupancy: a block is foreground when any voxel in it
    // exceeds the threshold. Walk the volume in memory order (contiguous, with
    // the rescale hoisted per slice) rather than scanning block by block.
    let mut fg = vec![0u8; cn];
    {
        let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
        let cx_of: Vec<usize> = (0..nx).map(|x| (x / stride[0]).min(cd[0] - 1)).collect();
        let cy_of: Vec<usize> = (0..ny).map(|y| (y / stride[1]).min(cd[1] - 1)).collect();
        let cz_of: Vec<usize> = (0..nz).map(|z| (z / stride[2]).min(cd[2] - 1)).collect();
        let nxy = nx * ny;
        for z in 0..nz {
            let (slope, intercept) = volume.rescale[z];
            let row_z = z * nxy;
            let cz = cz_of[z];
            for y in 0..ny {
                let row = row_z + y * nx;
                let base = (cz * cd[1] + cy_of[y]) * cd[0];
                for x in 0..nx {
                    if volume.data[row + x] * slope + intercept > threshold {
                        fg[base + cx_of[x]] = 1;
                    }
                }
            }
        }
    }

    // Morphological open then close on the coarse grid.
    if params.open_radius > 0 {
        fg = erode(&fg, cd, params.open_radius);
        fg = dilate(&fg, cd, params.open_radius);
    }
    if params.close_radius > 0 {
        fg = dilate(&fg, cd, params.close_radius);
        fg = erode(&fg, cd, params.close_radius);
    }

    // Head = connected component nearest the volume centre (the table is a long
    // component that usually touches the head, so we prefer the component that
    // contains the densest central block, then fall back to the largest).
    let mut head = select_head_component(&fg, cd, stride, volume, threshold, params)?;

    // Clip the head to a head-sized sphere around the centroid of its superior
    // portion, so a component that leaked into the neck/body is trimmed back to
    // the head. The superior centroid approximates the brain centre.
    clip_head_radius(&mut head, cd, stride, volume, params.head_radius_mm);

    // Brain estimate within the head.
    let (brain, skull, vault, cavity) =
        estimate_brain(volume, &head, cd, stride, threshold, params);

    // Centroids in patient space.
    let head_centroid = coarse_centroid(volume, &head, cd, stride);
    let brain_centroid = coarse_centroid(volume, &brain, cd, stride);

    Ok(Segmentation {
        dims: cd,
        stride,
        head,
        brain,
        skull,
        vault,
        cavity,
        origin_voxel: [0, 0, 0],
        head_centroid,
        brain_centroid,
    })
}

/// Pick the head component: the connected foreground component whose centroid
/// is closest to the volume's centre of mass, subject to a size window.
fn select_head_component(
    fg: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    volume: &Volume,
    threshold: f32,
    params: &SegParams,
) -> Result<Vec<u8>, String> {
    let cn = cd[0] * cd[1] * cd[2];
    let mut label = vec![0i32; cn];
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut components: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut next = 1i32;
    for start in 0..cn {
        if fg[start] == 0 || label[start] != 0 {
            continue;
        }
        let mut members = Vec::new();
        queue.clear();
        queue.push_back(start);
        label[start] = next;
        while let Some(i) = queue.pop_front() {
            members.push(i);
            let cz = i / (cd[0] * cd[1]);
            let rem = i % (cd[0] * cd[1]);
            let cy = rem / cd[0];
            let cx = rem % cd[0];
            let mut visit = |nx: isize, ny: isize, nz: isize, label: &mut Vec<i32>| {
                if nx < 0 || ny < 0 || nz < 0 {
                    return;
                }
                let nx = nx as usize;
                let ny = ny as usize;
                let nz = nz as usize;
                if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                    return;
                }
                let ni = (nz * cd[1] + ny) * cd[0] + nx;
                if fg[ni] != 0 && label[ni] == 0 {
                    label[ni] = next;
                    queue.push_back(ni);
                }
            };
            // 6-connectivity.
            visit(cx as isize - 1, cy as isize, cz as isize, &mut label);
            visit(cx as isize + 1, cy as isize, cz as isize, &mut label);
            visit(cx as isize, cy as isize - 1, cz as isize, &mut label);
            visit(cx as isize, cy as isize + 1, cz as isize, &mut label);
            visit(cx as isize, cy as isize, cz as isize - 1, &mut label);
            visit(cx as isize, cy as isize, cz as isize + 1, &mut label);
        }
        components.push((members.len(), members));
        next += 1;
    }

    if components.is_empty() {
        return Err("no foreground components found".to_string());
    }

    // Centre of the volume in patient space.
    let center_patient = volume.world(volume.nx() / 2, volume.ny() / 2, volume.nz() / 2);
    let min_cells = (params.min_head_fraction * cn as f64).max(1.0) as usize;
    let max_cells = (params.max_head_fraction * cn as f64) as usize;

    let mut best: Option<(f64, Vec<usize>)> = None;
    for (count, members) in components.into_iter() {
        if count < min_cells || count > max_cells {
            continue;
        }
        // Component centroid in coarse*stride voxels -> patient space.
        let mut c = [0.0f64; 3];
        for &i in &members {
            let cz = i / (cd[0] * cd[1]);
            let rem = i % (cd[0] * cd[1]);
            let cy = rem / cd[0];
            let cx = rem % cd[0];
            let p = volume.world(
                (cx * stride[0]).min(volume.nx() - 1),
                (cy * stride[1]).min(volume.ny() - 1),
                (cz * stride[2]).min(volume.nz() - 1),
            );
            c = add(c, p);
        }
        c = scale(c, 1.0 / members.len() as f64);
        // Score: prefer the component closest to the volume centre, and larger.
        let dist = dot(sub(c, center_patient), sub(c, center_patient)).sqrt();
        let score = dist - 0.05 * members.len() as f64;
        if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, members));
        }
    }
    // Fall back to the largest component if nothing passed the size window.
    let members = match best {
        Some((_, m)) => m,
        None => {
            // Recompute largest (cheap second pass over labels).
            let mut counts = std::collections::HashMap::new();
            for &l in &label {
                if l != 0 {
                    *counts.entry(l).or_insert(0usize) += 1;
                }
            }
            let (largest_label, _) = counts
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .ok_or_else(|| "no head component".to_string())?;
            (0..cn).filter(|&i| label[i] == largest_label).collect()
        }
    };

    let mut head = vec![0u8; cn];
    for i in members {
        head[i] = 1;
    }
    let _ = threshold;
    Ok(head)
}

/// Trim the head grid to a sphere of `radius_mm` around the centroid of its
/// superior 40% (an approximation of the brain centre).
fn clip_head_radius(
    head: &mut [u8],
    cd: [usize; 3],
    stride: [usize; 3],
    volume: &Volume,
    radius_mm: f64,
) {
    if radius_mm <= 0.0 {
        return;
    }
    // Superior axis is patient +Z. Find the head's Z extent and the centroid of
    // its top 40%.
    let mut zmin = f64::MAX;
    let mut zmax = f64::MIN;
    for (i, &v) in head.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let pz = volume.world(x, y, z)[2];
        zmin = zmin.min(pz);
        zmax = zmax.max(pz);
    }
    if zmin > zmax {
        return;
    }
    let z_cut = zmin + (zmax - zmin) * 0.6;
    let mut c = [0.0f64; 3];
    let mut n = 0.0f64;
    for (i, &v) in head.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let p = volume.world(x, y, z);
        if p[2] >= z_cut {
            c = add(c, p);
            n += 1.0;
        }
    }
    if n == 0.0 {
        return;
    }
    c = scale(c, 1.0 / n);

    for (i, v) in head.iter_mut().enumerate() {
        if *v == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let p = volume.world(x, y, z);
        let d = sub(p, c);
        if dot(d, d).sqrt() > radius_mm {
            *v = 0;
        }
    }
}

/// Detect the skull (CT): threshold bone, keep the largest connected bone
/// component, then morphologically close it to seal small gaps so it forms a
/// continuous barrier around the brain.
///
/// Returns the closed `skull` grid only. The **cranial vault** is derived
/// separately (see [`vault_from_cavity`]) as the part of the skull that actually
/// encloses the intracranial cavity, which cleanly excludes the facial skeleton
/// — connectivity alone cannot separate them because the facial bones fuse with
/// the calvarium.
fn detect_skull(
    volume: &Volume,
    head: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    params: &SegParams,
) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    let mut bone = vec![0u8; cn];
    let (vx, vy, vz) = coarse_axis_voxels(cd, stride, volume.dims);
    let (ny, nx) = (volume.ny(), volume.nx());
    for cz in 0..cd[2] {
        let z = vz[cz];
        let (slope, intercept) = volume.rescale[z];
        for cy in 0..cd[1] {
            let y = vy[cy];
            let row = (z * ny + y) * nx;
            let crow = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                let i = crow + cx;
                if head[i] == 0 {
                    continue;
                }
                if volume.data[row + vx[cx]] * slope + intercept >= params.bone_hu {
                    bone[i] = 1;
                }
            }
        }
    }
    // Close gaps (dilate a lot, then erode back) so the skull is continuous.
    if params.skull_close_radius > 0 {
        let r = params.skull_close_radius;
        bone = dilate(&bone, cd, r + 1);
        bone = erode(&bone, cd, r);
    }
    // Keep only the component containing the topmost bone voxel: the calvarium
    // is the most superior bony structure, so this drops disconnected specks and
    // (usually) the facial bones when they are separate. When they fuse, the
    // vault is still separated by `vault_from_cavity`.
    component_of_extreme(&bone, cd, stride, volume, |p| p[2], true)
}

/// Derive the cranial vault from the intracranial cavity: the vault is the skull
/// shell that surrounds the cavity. Bone voxels within `radius` of the cavity
/// belong to the vault; bone elsewhere (e.g. the facial skeleton, mandible, or
/// the pituitary/ skull base anterior to the cavity) does not.
fn vault_from_cavity(cavity: &[u8], skull: &[u8], cd: [usize; 3], radius: u32) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    if cavity.iter().all(|&b| b == 0) || skull.iter().all(|&b| b == 0) {
        return vec![0u8; cn];
    }
    // A closed shell just outside the cavity: dilate the cavity, keep the skull
    // that falls in the shell. Two-pass dilation gives a shell of the requested
    // thickness while never including bone interior to the cavity.
    let near = dilate(cavity, cd, radius.max(1));
    let mut vault = vec![0u8; cn];
    for i in 0..cn {
        if skull[i] != 0 && near[i] != 0 {
            vault[i] = 1;
        }
    }
    // Keep the vault component that encloses the cavity (largest is fine: the
    // shell is contiguous around the brain).
    largest_component(&vault, cd)
}

/// The protected intracranial core: the intracranial contents (`cavity`, or
/// `brain` when no cavity was grown) plus the vault, padded by a **physical**
/// `protect_mm` safety band.
///
/// The padding is per-axis and measured in millimetres (nearest coarse cell), so
/// a thick-slice CT does not turn a 2 mm margin into a whole-slice (~5 mm) or
/// multi-slice band. That was leaving a shell of renderable scalp around the
/// vault.
fn protected_core(
    cavity: &[u8],
    vault: &[u8],
    brain: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    spacing: [f64; 3],
    protect_mm: f64,
) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    let has_vault = vault.iter().any(|&v| v != 0);
    let has_cavity = cavity.iter().any(|&v| v != 0);
    let seed: &[u8] = if has_cavity { cavity } else { brain };
    let mut core = vec![0u8; cn];
    for i in 0..cn {
        if seed[i] != 0 || (has_vault && vault[i] != 0) {
            core[i] = 1;
        }
    }
    // `0` means no extra padding (the caller wants the tightest core); a
    // positive value widens it by that many millimetres.
    let band_mm = protect_mm.max(0.0);
    dilate_axes(&core, cd, cell_radius(band_mm, stride, spacing))
}

/// Per-axis cell counts for a physical distance (nearest cell, not rounded up,
/// so a band cannot balloon on thick slices).
fn cell_radius(mm: f64, stride: [usize; 3], spacing: [f64; 3]) -> [usize; 3] {
    let mut r = [0usize; 3];
    for a in 0..3 {
        let cell = (stride[a] as f64 * spacing[a]).abs().max(1e-3);
        r[a] = if mm <= 0.0 {
            0
        } else {
            (mm / cell).round() as usize
        };
    }
    r
}

/// Feather the removal weight of a deflesh mask so the keep boundary fades
/// instead of ending in a hard wall.
///
/// For every removed voxel, the weight ramps linearly from `0` at the nearest
/// kept voxel to `1` at `smooth_mm` inside the removed region. Distance is
/// measured with a 6-connected multi-source BFS seeded from every removed voxel
/// that neighbours a kept voxel (or the volume border) — i.e. the boundary of
/// the removed set. This leaves the binary `remove` flags untouched, so the
/// safety guarantees (0 brain / 0 vault removed) are unchanged; only the
/// rendering strength at the edge is softened. The optional `core` (coarse keep
/// grid) is used to respect the true keep boundary, which is finer than the
/// binary mask on the coarse grid.
fn feather_boundary(
    mask: &mut Mask,
    volume: &Volume,
    smooth_mm: f64,
    core: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    bbox: [usize; 6],
) {
    let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
    if nx == 0 || ny == 0 || nz == 0 {
        return;
    }
    // Voxel-scale distance in cells along each axis (the feather is isotropic in
    // millimetres, so the per-axis step differs when spacing is anisotropic).
    let step = [
        ((volume.spacing[0].abs().max(1e-3) / smooth_mm).max(1e-6)) as f32,
        ((volume.spacing[1].abs().max(1e-3) / smooth_mm).max(1e-6)) as f32,
        ((volume.spacing[2].abs().max(1e-3) / smooth_mm).max(1e-6)) as f32,
    ];

    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    // The weight buffer doubles as the distance field: it starts at `1.0`
    // (fully removed, the clamp value) and boundary seeds drop to `0`. Because
    // weights never exceed `1.0`, the BFS can stop after one feather width,
    // which avoids a second full-volume distance buffer and keeps the queue
    // limited to the boundary band.
    mask.ensure_weight();
    let mut queue: VecDeque<(usize, f32)> = VecDeque::new();

    // Only removed voxels can seed the BFS, so restrict the scan to their
    // bounding box. Cell lookups use per-axis maps to avoid divisions.
    let cx_of: Vec<usize> = (0..nx).map(|x| (x / stride[0]).min(cd[0] - 1)).collect();
    let cy_of: Vec<usize> = (0..ny).map(|y| (y / stride[1]).min(cd[1] - 1)).collect();
    let cz_of: Vec<usize> = (0..nz).map(|z| (z / stride[2]).min(cd[2] - 1)).collect();
    let (x0, x1, y0, y1, z0, z1) = (
        bbox[0].min(nx - 1),
        bbox[1].min(nx - 1),
        bbox[2].min(ny - 1),
        bbox[3].min(ny - 1),
        bbox[4].min(nz - 1),
        bbox[5].min(nz - 1),
    );

    // Seed: removed voxels adjacent to a kept voxel (or the volume border), or
    // adjacent to a coarse keep cell (the true boundary is the coarse `core`).
    for z in z0..=z1 {
        let cz = cz_of[z];
        for y in y0..=y1 {
            let cy = cy_of[y];
            let core_row = (cz * cd[1] + cy) * cd[0];
            for x in x0..=x1 {
                let i = idx(x, y, z);
                if mask.remove[i] == 0 {
                    continue;
                }
                let cx = cx_of[x];
                // A removed voxel sitting on a coarse keep cell is boundary.
                let mut boundary = !core.is_empty() && core[core_row + cx] != 0;
                if !boundary {
                    for (dx, dy, dz) in NEIGHBORS6 {
                        let (ox, oy, oz) = (x as isize + dx, y as isize + dy, z as isize + dz);
                        if ox < 0 || oy < 0 || oz < 0 {
                            boundary = true;
                            break;
                        }
                        let (ox, oy, oz) = (ox as usize, oy as usize, oz as usize);
                        if ox >= nx || oy >= ny || oz >= nz {
                            boundary = true;
                            break;
                        }
                        if mask.remove[idx(ox, oy, oz)] == 0 {
                            boundary = true;
                            break;
                        }
                    }
                }
                if boundary {
                    mask.weight[i] = 0.0;
                    queue.push_back((i, 0.0));
                }
            }
        }
    }

    // 6-connected BFS with anisotropic step cost; keep the min distance.
    // Distances only matter below `1.0`; beyond that the weight clamps to `1.0`
    // (the initial value), so propagation stops there.
    while let Some((i, d)) = queue.pop_front() {
        if d >= 1.0 || d > mask.weight[i] {
            continue;
        }
        let x = i % nx;
        let y = (i / nx) % ny;
        let z = i / (nx * ny);
        for (k, (dx, dy, dz)) in NEIGHBORS6.iter().enumerate() {
            let cost = step[k / 2];
            let (ox, oy, oz) = (x as isize + dx, y as isize + dy, z as isize + dz);
            if ox < 0 || oy < 0 || oz < 0 {
                continue;
            }
            let (ox, oy, oz) = (ox as usize, oy as usize, oz as usize);
            if ox >= nx || oy >= ny || oz >= nz {
                continue;
            }
            let j = idx(ox, oy, oz);
            if mask.remove[j] == 0 {
                continue;
            }
            let nd = d + cost;
            if nd < mask.weight[j] {
                mask.weight[j] = nd;
                if nd < 1.0 {
                    queue.push_back((j, nd));
                }
            }
        }
    }
}

/// The 6 face-neighbours `(dx, dy, dz)`; index/2 gives the axis (0=x,1=y,2=z).
const NEIGHBORS6: [(isize, isize, isize); 6] = [
    (-1, 0, 0),
    (1, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
];

/// The connected component (of a coarse binary grid) that contains the voxel
/// extreme in some direction, chosen by `score` (maximised when `maximize`).
fn component_of_extreme<F: Fn(V3) -> f64>(
    grid: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    volume: &Volume,
    score: F,
    maximize: bool,
) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    let mut seed = None;
    let mut best = if maximize { f64::MIN } else { f64::MAX };
    for i in 0..cn {
        if grid[i] == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let s = score(volume.world(x, y, z));
        if (maximize && s > best) || (!maximize && s < best) {
            best = s;
            seed = Some(i);
        }
    }
    let Some(seed) = seed else {
        return vec![0u8; cn];
    };
    // Flood from the seed.
    let mut out = vec![0u8; cn];
    let mut queue: VecDeque<usize> = VecDeque::new();
    queue.push_back(seed);
    out[seed] = 1;
    while let Some(i) = queue.pop_front() {
        let cz = i / (cd[0] * cd[1]);
        let rem = i % (cd[0] * cd[1]);
        let cy = rem / cd[0];
        let cx = rem % cd[0];
        let mut visit = |nx: isize, ny: isize, nz: isize, out: &mut Vec<u8>| {
            if nx < 0 || ny < 0 || nz < 0 {
                return;
            }
            let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
            if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                return;
            }
            let ni = (nz * cd[1] + ny) * cd[0] + nx;
            if grid[ni] != 0 && out[ni] == 0 {
                out[ni] = 1;
                queue.push_back(ni);
            }
        };
        visit(cx as isize - 1, cy as isize, cz as isize, &mut out);
        visit(cx as isize + 1, cy as isize, cz as isize, &mut out);
        visit(cx as isize, cy as isize - 1, cz as isize, &mut out);
        visit(cx as isize, cy as isize + 1, cz as isize, &mut out);
        visit(cx as isize, cy as isize, cz as isize - 1, &mut out);
        visit(cx as isize, cy as isize, cz as isize + 1, &mut out);
    }
    out
}

/// Keep only the largest 6-connected component of a coarse grid.
fn largest_component(grid: &[u8], cd: [usize; 3]) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    let mut label = vec![0i32; cn];
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut best: Vec<usize> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut next = 1i32;
    for start in 0..cn {
        if grid[start] == 0 || label[start] != 0 {
            continue;
        }
        current.clear();
        queue.clear();
        queue.push_back(start);
        label[start] = next;
        while let Some(i) = queue.pop_front() {
            current.push(i);
            let cz = i / (cd[0] * cd[1]);
            let rem = i % (cd[0] * cd[1]);
            let cy = rem / cd[0];
            let cx = rem % cd[0];
            let mut visit = |nx: isize, ny: isize, nz: isize, label: &mut Vec<i32>| {
                if nx < 0 || ny < 0 || nz < 0 {
                    return;
                }
                let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                    return;
                }
                let ni = (nz * cd[1] + ny) * cd[0] + nx;
                if grid[ni] != 0 && label[ni] == 0 {
                    label[ni] = next;
                    queue.push_back(ni);
                }
            };
            visit(cx as isize - 1, cy as isize, cz as isize, &mut label);
            visit(cx as isize + 1, cy as isize, cz as isize, &mut label);
            visit(cx as isize, cy as isize - 1, cz as isize, &mut label);
            visit(cx as isize, cy as isize + 1, cz as isize, &mut label);
            visit(cx as isize, cy as isize, cz as isize - 1, &mut label);
            visit(cx as isize, cy as isize, cz as isize + 1, &mut label);
        }
        if current.len() > best.len() {
            std::mem::swap(&mut best, &mut current);
        }
        next += 1;
    }
    let mut out = vec![0u8; cn];
    for i in best {
        out[i] = 1;
    }
    out
}

fn estimate_brain(
    volume: &Volume,
    head: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    threshold: f32,
    params: &SegParams,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let is_ct = volume.modality.eq_ignore_ascii_case("CT");
    let cn = cd[0] * cd[1] * cd[2];

    // On CT, detect and close the skull first; it is the brain grow barrier.
    let skull = if is_ct {
        detect_skull(volume, head, cd, stride, params)
    } else {
        vec![0u8; cn]
    };

    // Head intensity range for MR band.
    let (vx, vy, vz) = coarse_axis_voxels(cd, stride, volume.dims);
    let (nx, ny) = (volume.nx(), volume.ny());
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for cz in 0..cd[2] {
        let z = vz[cz];
        let (slope, intercept) = volume.rescale[z];
        let row_z = z * ny * nx;
        for cy in 0..cd[1] {
            let row = row_z + vy[cy] * nx;
            let crow = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                if head[crow + cx] == 0 {
                    continue;
                }
                let v = volume.data[row + vx[cx]] * slope + intercept;
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
    }
    let band = params.mr_brain_band;
    let mr_lo = lo + (hi - lo) * band.0 as f32;
    let mr_hi = lo + (hi - lo) * band.1 as f32;

    // Seed: coarse head voxel nearest the head centroid that is *not* skull
    // (on CT), so growth starts inside the cranial cavity.
    let head_centroid = coarse_centroid(volume, head, cd, stride);
    let mut seed = 0usize;
    let mut best_d = f64::MAX;
    for cz in 0..cd[2] {
        for cy in 0..cd[1] {
            let crow = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                let i = crow + cx;
                if head[i] == 0 {
                    continue;
                }
                if is_ct && skull[i] != 0 {
                    continue;
                }
                let p = volume.world(vx[cx], vy[cy], vz[cz]);
                let d = dot(sub(p, head_centroid), sub(p, head_centroid));
                if d < best_d {
                    best_d = d;
                    seed = i;
                }
            }
        }
    }

    // Grow the **intracranial cavity**: soft tissue/CSF reachable from the seed
    // within the head, bounded by bone (CT) or the MR intensity band. This is the
    // full space inside the skull, not just the brain, so it reaches the inner
    // table and lets us recognise the vault shell that surrounds it.
    let mut cavity = vec![0u8; cn];
    let mut queue: VecDeque<usize> = VecDeque::new();
    queue.push_back(seed);
    cavity[seed] = 1;
    // Intensity test for a candidate coarse cell at cell coordinates
    // `(cx, cy, cz)`: no division per neighbour, and the rescale is looked up
    // from the voxel slice.
    let accept = |cx: usize, cy: usize, cz: usize, i: usize| -> bool {
        if is_ct && skull[i] != 0 {
            return false; // skull is a hard barrier
        }
        let z = vz[cz];
        let (slope, intercept) = volume.rescale[z];
        let v = volume.data[(z * ny + vy[cy]) * nx + vx[cx]] * slope + intercept;
        if is_ct {
            // Soft tissue / CSF inside the skull.
            v > -100.0 && v < 200.0
        } else {
            v >= mr_lo && v <= mr_hi
        }
    };
    let _ = threshold;
    while let Some(i) = queue.pop_front() {
        let cz = i / (cd[0] * cd[1]);
        let rem = i % (cd[0] * cd[1]);
        let cy = rem / cd[0];
        let cx = rem % cd[0];
        let mut visit = |ncx: isize, ncy: isize, ncz: isize, grid: &mut Vec<u8>| {
            if ncx < 0 || ncy < 0 || ncz < 0 {
                return;
            }
            let (ncx, ncy, ncz) = (ncx as usize, ncy as usize, ncz as usize);
            if ncx >= cd[0] || ncy >= cd[1] || ncz >= cd[2] {
                return;
            }
            let ni = (ncz * cd[1] + ncy) * cd[0] + ncx;
            if head[ni] != 0 && grid[ni] == 0 && accept(ncx, ncy, ncz, ni) {
                grid[ni] = 1;
                queue.push_back(ni);
            }
        };
        visit(cx as isize - 1, cy as isize, cz as isize, &mut cavity);
        visit(cx as isize + 1, cy as isize, cz as isize, &mut cavity);
        visit(cx as isize, cy as isize - 1, cz as isize, &mut cavity);
        visit(cx as isize, cy as isize + 1, cz as isize, &mut cavity);
        visit(cx as isize, cy as isize, cz as isize - 1, &mut cavity);
        visit(cx as isize, cy as isize, cz as isize + 1, &mut cavity);
    }
    cavity = dilate(&cavity, cd, 1);
    cavity = erode(&cavity, cd, 1);

    // The cranial vault is precisely the skull shell that surrounds the cavity.
    // This separates the calvarium from the facial skeleton even when the two
    // fuse into one connected bone component.
    let vault = if is_ct {
        vault_from_cavity(&cavity, &skull, cd, params.vault_shell_radius)
    } else {
        vec![0u8; cn]
    };

    // Brain = cavity clamped to a brain-sized **ellipsoid** (not sphere) around
    // its own centroid. On CT the soft-tissue grow can still leak through thin
    // skull or a sinus, so this bounds it to intracranial scale (A-P shorter than
    // S-I). The ellipsoid is generous: it must not clip the frontal poles or
    // anterior temporal lobes, which are inside the cavity anyway.
    let brain_center = coarse_centroid(volume, &cavity, cd, stride);
    let brain_semi = [70.0f64, 80.0, 75.0]; // half-extents (mm) anterior/left/superior
    let mut brain = cavity.clone();
    for cz in 0..cd[2] {
        for cy in 0..cd[1] {
            let crow = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                let i = crow + cx;
                if brain[i] == 0 {
                    continue;
                }
                let p = volume.world(vx[cx], vy[cy], vz[cz]);
                let d = sub(p, brain_center);
                let e = (d[0] / brain_semi[0]).powi(2)
                    + (d[1] / brain_semi[1]).powi(2)
                    + (d[2] / brain_semi[2]).powi(2);
                if e > 1.0 {
                    brain[i] = 0;
                }
            }
        }
    }
    (brain, skull, vault, cavity)
}

/// Per-axis voxel index for every coarse cell along that axis. Hot coarse-grid
/// loops index these instead of dividing for every cell.
fn coarse_axis_voxels(
    cd: [usize; 3],
    stride: [usize; 3],
    dims: [usize; 3],
) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let map = |cells: usize, stride: usize, dim: usize| -> Vec<usize> {
        (0..cells).map(|c| (c * stride).min(dim - 1)).collect()
    };
    (
        map(cd[0], stride[0], dims[0]),
        map(cd[1], stride[1], dims[1]),
        map(cd[2], stride[2], dims[2]),
    )
}

/// An anterior, orbit-level region in patient coordinates, used by the optional
/// "include orbits" extension of the face cut.
///
/// The globes sit in front of the brain's anterior pole and below its
/// mid-height, so the region is derived from the segmented brain's
/// patient-space bounding box. It is deliberately shallow in Y: the globes are
/// anterior to the frontal pole, and reaching further back would remove the
/// temporal fossa.
#[derive(Clone, Copy, Debug)]
struct OrbitRegion {
    /// Anterior limit: voxels with `y < y_max` are inside (patient Y grows
    /// posteriorly).
    y_max: f64,
    /// Inferior/superior extent in patient Z.
    z: [f64; 2],
    /// Patient X of the midline.
    x_mid: f64,
    /// Lateral distance from the midline: `[inner, outer]`.
    x: [f64; 2],
}

impl OrbitRegion {
    fn contains(&self, p: [f64; 3]) -> bool {
        let lateral = (p[0] - self.x_mid).abs();
        p[1] < self.y_max
            && p[2] >= self.z[0]
            && p[2] <= self.z[1]
            && lateral >= self.x[0]
            && lateral <= self.x[1]
    }
}

/// Build the orbit region from the segmented brain.
fn orbit_region(volume: &Volume, seg: &Segmentation) -> Option<OrbitRegion> {
    let (lo, hi) = patient_bbox(volume, &seg.brain, seg.dims, seg.stride)?;
    let z_mid = 0.5 * (lo[2] + hi[2]);
    let z_half = (0.5 * (hi[2] - lo[2])).max(1.0);
    let x_half = (0.5 * (hi[0] - lo[0])).max(1.0);
    Some(OrbitRegion {
        // Strictly in front of the brain's anterior pole (with a small buffer):
        // the globes sit anterior to the pole, so this removes them while making
        // it impossible to reach brain tissue, even where the brain estimate is
        // clipped at the frontal poles.
        y_max: lo[1] - 4.0,
        // From just below the brain's mid-height down through the orbits.
        z: [z_mid - 0.55 * z_half, z_mid],
        // Lateral to the midline (skip the nasal cavity) and inside the head's
        // width, where the orbits sit.
        x_mid: 0.5 * (lo[0] + hi[0]),
        x: [12.0, (0.90 * x_half).max(40.0)],
    })
}

/// Ear-height, posterior-of-face band in patient coordinates, used by the
/// optional "include ears" extension of the face cut.
///
/// The auricles are external tissue lateral to the vault, roughly at the level of
/// the temporal lobes and between the face and the occiput. The band is a box;
/// the protected core still guarantees that no brain or vault voxel is removed.
/// It extends back past the full auricle (including the lobe) while staying in
/// front of the occiput.
#[derive(Clone, Copy, Debug)]
struct EarRegion {
    /// Posterior limit: voxels with `y < y_max` are inside (patient Y grows
    /// posteriorly). Stops in front of the occiput so the nape/back of the head
    /// stays.
    y_max: f64,
    /// Inferior/superior extent in patient Z (ear canal to top of auricle).
    z: [f64; 2],
    /// Patient X of the midline.
    x_mid: f64,
    /// Lateral distance from the midline beyond which the band applies. Keeps
    /// the midline/occipital scalp while removing the temporal scalp and ears.
    x_min: f64,
}

impl EarRegion {
    fn contains(&self, p: [f64; 3]) -> bool {
        let lateral = (p[0] - self.x_mid).abs();
        p[1] < self.y_max && p[2] >= self.z[0] && p[2] <= self.z[1] && lateral >= self.x_min
    }
}

/// Build the ear band from the segmented brain.
fn ear_region(volume: &Volume, seg: &Segmentation) -> Option<EarRegion> {
    let (lo, hi) = patient_bbox(volume, &seg.brain, seg.dims, seg.stride)?;
    let y_extent = (hi[1] - lo[1]).max(1.0);
    let z_mid = 0.5 * (lo[2] + hi[2]);
    let z_half = (0.5 * (hi[2] - lo[2])).max(1.0);
    let x_half = (0.5 * (hi[0] - lo[0])).max(1.0);
    Some(EarRegion {
        // From the face cut back past the full auricle. The occiput and nape
        // (posterior to this) are preserved.
        y_max: lo[1] + 0.85 * y_extent,
        // Ear lobe through the top of the auricle.
        z: [z_mid - 0.45 * z_half, z_mid + 0.55 * z_half],
        // Lateral half of the head only: ears and temporal scalp, not the
        // midline/occipital scalp.
        x_mid: 0.5 * (lo[0] + hi[0]),
        x_min: 0.65 * x_half,
    })
}

/// Lateral midface / skull-base region in patient coordinates, used by the
/// optional "include zygoma" extension of the face cut.
///
/// The zygomatic bone and arch lie lateral to the maxilla and orbit, below the
/// orbital floor, and run backwards to the temporomandibular joint. A flat cut
/// plane in front of the brain leaves them (they are behind it), so this region
/// removes them together with the lateral skull base around their root. The
/// guard keeps the brain and CSF, so the temporal lobe is never reached.
#[derive(Clone, Copy, Debug)]
struct ZygomaRegion {
    /// Posterior limit: voxels with `y < y_max` are inside (patient Y grows
    /// posteriorly). Reaches past the temporomandibular joint into the lateral
    /// skull base, while stopping in front of the occiput.
    y_max: f64,
    /// Inferior/superior extent in patient Z: from below the maxilla up to the
    /// floor of the middle cranial fossa.
    z: [f64; 2],
    /// Patient X of the midline.
    x_mid: f64,
    /// Lateral distance from the midline beyond which the region applies. Keeps
    /// the midline midface to the plane cut and targets the cheek/arch.
    x_min: f64,
}

impl ZygomaRegion {
    fn contains(&self, p: [f64; 3]) -> bool {
        let lateral = (p[0] - self.x_mid).abs();
        p[1] < self.y_max && p[2] >= self.z[0] && p[2] <= self.z[1] && lateral >= self.x_min
    }
}

/// Build the zygoma / lateral skull-base region from the segmented brain.
fn zygoma_region(volume: &Volume, seg: &Segmentation) -> Option<ZygomaRegion> {
    let (lo, hi) = patient_bbox(volume, &seg.brain, seg.dims, seg.stride)?;
    let y_extent = (hi[1] - lo[1]).max(1.0);
    let z_mid = 0.5 * (lo[2] + hi[2]);
    let z_half = (0.5 * (hi[2] - lo[2])).max(1.0);
    let x_half = (0.5 * (hi[0] - lo[0])).max(1.0);
    Some(ZygomaRegion {
        // From the face back past the temporomandibular joint into the lateral
        // skull base (the root of the arch), but well in front of the occiput.
        y_max: seg.brain_centroid[1] + 0.05 * y_extent,
        // Strictly below the orbital floor (the orbit option owns that level)
        // down to the alveolar process. Staying below the orbit keeps the whole
        // region under the inferior temporal lobes rather than beside them.
        z: [z_mid - 1.05 * z_half, z_mid - 0.45 * z_half],
        x_mid: 0.5 * (lo[0] + hi[0]),
        // Lateral to the medial orbital wall / nasal cavity: the cheek and arch.
        x_min: 0.45 * x_half,
    })
}

/// Voxel bounding box `[x0, x1, y0, y1, z0, z1]` of the removed voxels, or
/// `None` when nothing is removed. Used to restrict the continuity passes to the
/// defaced neighbourhood instead of the whole volume.
fn removal_bbox(mask: &Mask) -> Option<[usize; 6]> {
    let (nx, ny, nz) = (mask.dims[0], mask.dims[1], mask.dims[2]);
    let nxy = nx * ny;
    let (mut lo, mut hi) = ([usize::MAX; 3], [0usize; 3]);
    for z in 0..nz {
        let zbase = z * nxy;
        for y in 0..ny {
            let row = zbase + y * nx;
            let mut hits = false;
            for x in 0..nx {
                if mask.remove[row + x] != 0 {
                    hits = true;
                    lo[0] = lo[0].min(x);
                    hi[0] = hi[0].max(x);
                }
            }
            if hits {
                lo[1] = lo[1].min(y);
                hi[1] = hi[1].max(y);
                lo[2] = lo[2].min(z);
                hi[2] = hi[2].max(z);
            }
        }
    }
    if lo[0] == usize::MAX {
        None
    } else {
        Some([lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]])
    }
}

/// Thickness below which a kept structure embedded in the removal is dropped
/// (see [`remove_thin_kept_runs`]). A few millimetres: enough to clear the
/// one-voxel layers a cut boundary leaves and thin bone lamellae, small enough to
/// leave real anatomy along the kept boundary alone.
const THIN_KEPT_MM: f64 = 8.0;

/// Strict brain/vault cells (no safety padding and no `cavity` grid, which can
/// leak into the face). Small kept islands inside the removal are only filled
/// when they are outside these cells.
fn strict_core_guard(seg: &Segmentation) -> Vec<u8> {
    let cn = seg.dims[0] * seg.dims[1] * seg.dims[2];
    let mut guard = vec![0u8; cn];
    for i in 0..cn {
        if seg.brain[i] != 0 || seg.vault[i] != 0 {
            guard[i] = 1;
        }
    }
    guard
}

/// Like [`strict_core_guard`] but also protects the segmented CSF (`cavity`).
/// The sliver pass uses it so a thin sheet right on the brain surface (dura/CSF)
/// is never dropped, which would expose the brain.
fn strict_core_and_cavity_guard(seg: &Segmentation) -> Vec<u8> {
    let cn = seg.dims[0] * seg.dims[1] * seg.dims[2];
    let mut guard = vec![0u8; cn];
    for i in 0..cn {
        if seg.brain[i] != 0 || seg.vault[i] != 0 || seg.cavity[i] != 0 {
            guard[i] = 1;
        }
    }
    guard
}

/// Fill small **kept** foreground islands inside the removal region (e.g. little
/// specks of tissue left by the coarse guards), up to `max_size` voxels.
///
/// Only components that border the removal are candidates, so filling them never
/// splits the defaced area into separate pieces. Components touching the edge of
/// the removal box or intersecting the strict brain/vault guard are left alone,
/// so the main kept body and the brain are never removed.
fn fill_small_kept_islands(
    mask: &mut Mask,
    volume: &Volume,
    threshold: f32,
    core: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    bbox: [usize; 6],
    max_size: usize,
) -> usize {
    let dims = volume.dims;
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let nxy = nx * ny;
    if nx == 0 || ny == 0 || nz == 0 || bbox[0] > bbox[1] {
        return 0;
    }
    // An island that borders the removal lies within (at most a voxel outside)
    // the removal box, so only this sub-volume needs scanning.
    let lo = [bbox[0], bbox[2], bbox[4]];
    let hi = [bbox[1], bbox[3], bbox[5]];
    let (lx, ly, lz) = (hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1);
    let lxy = lx * ly;
    // Precompute foreground once: the BFS otherwise rescales a voxel for every
    // neighbour check, which dominates the pass on large volumes.
    let mut fg = vec![0u8; lxy * lz];
    for (lz_i, z) in (lo[2]..=hi[2]).enumerate() {
        let (slope, intercept) = volume.rescale[z];
        let vbase = z * nxy;
        let fbase = lz_i * lxy;
        for (ly_i, y) in (lo[1]..=hi[1]).enumerate() {
            let vrow = vbase + y * nx;
            let frow = fbase + ly_i * lx;
            for lx_i in 0..lx {
                if volume.data[vrow + lo[0] + lx_i] * slope + intercept > threshold {
                    fg[frow + lx_i] = 1;
                }
            }
        }
    }
    // 0 = unvisited, 1 = in the component being walked, 2 = settled (part of a
    // large body, already filled, or otherwise not a fillable island). The walk
    // abandons anything larger than `max_size`, and records that verdict on the
    // voxels it did visit, so a later walk that touches them is abandoned too
    // instead of being mistaken for a small enclosed island.
    let mut state = vec![0u8; fg.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut members: Vec<usize> = Vec::new();
    let mut filled = 0usize;
    for lz_i in 0..lz {
        for ly_i in 0..ly {
            let frow = lz_i * lxy + ly_i * lx;
            for lx_i in 0..lx {
                let start = frow + lx_i;
                if state[start] != 0 || fg[start] == 0 {
                    continue;
                }
                let (gx0, gy0, gz0) = (lo[0] + lx_i, lo[1] + ly_i, lo[2] + lz_i);
                if mask.remove[gz0 * nxy + gy0 * nx + gx0] != 0 {
                    continue;
                }
                stack.clear();
                members.clear();
                stack.push(start);
                state[start] = 1;
                // An island counts only if it actually borders the removal, so
                // filling it keeps the defaced area one connected region. Touching
                // the removal box edge (`border`) means it is part of the main
                // kept body; the brain/vault guard is never touched.
                let (mut border, mut along_removal, mut protected, mut large) =
                    (false, false, false, false);
                while let Some(i) = stack.pop() {
                    members.push(i);
                    if members.len() > max_size {
                        large = true;
                        break;
                    }
                    let (iz, ir) = (i / lxy, i % lxy);
                    let (iy, ix) = (ir / lx, ir % lx);
                    if ix == 0 || iy == 0 || iz == 0 || ix + 1 == lx || iy + 1 == ly || iz + 1 == lz
                    {
                        border = true;
                    }
                    let gx = lo[0] + ix;
                    let gy = lo[1] + iy;
                    let gz = lo[2] + iz;
                    let cell = (gz / stride[2]).min(cd[2] - 1) * cd[1] * cd[0]
                        + (gy / stride[1]).min(cd[1] - 1) * cd[0]
                        + (gx / stride[0]).min(cd[0] - 1);
                    if core[cell] != 0 {
                        protected = true;
                    }
                    for (dx, dy, dz) in [
                        (-1i64, 0i64, 0i64),
                        (1, 0, 0),
                        (0, -1, 0),
                        (0, 1, 0),
                        (0, 0, -1),
                        (0, 0, 1),
                    ] {
                        let (ax, ay, az) = (ix as i64 + dx, iy as i64 + dy, iz as i64 + dz);
                        if ax < 0
                            || ay < 0
                            || az < 0
                            || ax as usize >= lx
                            || ay as usize >= ly
                            || az as usize >= lz
                        {
                            border = true;
                            continue;
                        }
                        let j = (az as usize * ly + ay as usize) * lx + ax as usize;
                        let g = (lo[2] + az as usize) * nxy
                            + (lo[1] + ay as usize) * nx
                            + lo[0]
                            + ax as usize;
                        if mask.remove[g] != 0 {
                            // Removed boundary; do not walk into the removal.
                            along_removal = true;
                            continue;
                        }
                        if fg[j] == 0 {
                            continue;
                        }
                        match state[j] {
                            0 => {
                                state[j] = 1;
                                stack.push(j);
                            }
                            2 => large = true,
                            _ => {}
                        }
                    }
                }
                if large {
                    // The walk stopped mid-frontier; those voxels belong to the
                    // same too-large component, so settle them as well.
                    for &i in &stack {
                        state[i] = 2;
                    }
                }
                if !large && !border && along_removal && !protected {
                    let mut n = 0usize;
                    for &i in &members {
                        let (iz, ir) = (i / lxy, i % lxy);
                        let (iy, ix) = (ir / lx, ir % lx);
                        let g = (lo[2] + iz) * nxy + (lo[1] + iy) * nx + lo[0] + ix;
                        if mask.remove[g] == 0 {
                            mask.remove[g] = 1;
                            n += 1;
                        }
                    }
                    filled += n;
                }
                for &i in &members {
                    state[i] = 2;
                }
            }
        }
    }
    filled
}

/// Remove thin kept sheets and fingers that survive inside the defaced area.
///
/// A maximal run of kept foreground along an axis is dropped when it is thinner
/// than `max_thickness_mm` and bounded by removed voxels at **both** ends. That
/// catches the leftovers a region boundary leaves behind (a one-voxel layer where
/// a cut crosses the scalp, a bone lamella poking into the face, a sliver of
/// auricle at the ear boundary) while leaving alone the main kept body, anything
/// with air on one side, and the brain/vault guard. Because each dropped run is
/// bounded by removed voxels on both sides, the defaced area stays continuous.
fn remove_thin_kept_runs(
    mask: &mut Mask,
    volume: &Volume,
    threshold: f32,
    core: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
    bbox: [usize; 6],
    max_thickness_mm: f64,
) -> usize {
    let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
    let nxy = nx * ny;
    if nx == 0 || ny == 0 || nz == 0 || bbox[0] > bbox[1] {
        return 0;
    }
    let (x0, x1) = (bbox[0], bbox[1]);
    let (y0, y1) = (bbox[2], bbox[3]);
    let (z0, z1) = (bbox[4], bbox[5]);
    let lo = [x0, y0, z0];
    let hi = [x1, y1, z1];
    let (lx, ly, lz) = (x1 - x0 + 1, y1 - y0 + 1, z1 - z0 + 1);
    let lxy = lx * ly;

    // Foreground for the removal box only.
    let mut fg = vec![0u8; lxy * lz];
    for (lz_i, z) in (z0..=z1).enumerate() {
        let (slope, intercept) = volume.rescale[z];
        let vbase = z * nxy;
        let fbase = lz_i * lxy;
        for (ly_i, y) in (y0..=y1).enumerate() {
            let vrow = vbase + y * nx;
            let frow = fbase + ly_i * lx;
            for lx_i in 0..lx {
                if volume.data[vrow + x0 + lx_i] * slope + intercept > threshold {
                    fg[frow + lx_i] = 1;
                }
            }
        }
    }
    // A kept foreground voxel: above the threshold and not removed.
    let kept = |x: usize, y: usize, z: usize| -> bool {
        let l = ((z - z0) * ly + (y - y0)) * lx + (x - x0);
        fg[l] != 0 && mask.remove[(z * ny + y) * nx + x] == 0
    };

    // Longest kept run (in voxels) still considered "thin" along each axis.
    let max_run = [
        (max_thickness_mm / volume.spacing[0].max(1e-3))
            .ceil()
            .max(1.0) as usize,
        (max_thickness_mm / volume.spacing[1].max(1e-3))
            .ceil()
            .max(1.0) as usize,
        (max_thickness_mm / volume.spacing[2].max(1e-3))
            .ceil()
            .max(1.0) as usize,
    ];

    let mut thin = vec![0u8; lxy * lz];
    for axis in 0..3 {
        let (d0, d1) = match axis {
            0 => (1usize, 2usize),
            1 => (0usize, 2usize),
            _ => (0usize, 1usize),
        };
        let limit = max_run[axis];
        let mut c = [0usize; 3];
        for b1 in lo[d1]..=hi[d1] {
            for b0 in lo[d0]..=hi[d0] {
                c[d0] = b0;
                c[d1] = b1;
                let mut k = lo[axis];
                while k <= hi[axis] {
                    c[axis] = k;
                    if !kept(c[0], c[1], c[2]) {
                        k += 1;
                        continue;
                    }
                    let start = k;
                    while k <= hi[axis] {
                        c[axis] = k;
                        if !kept(c[0], c[1], c[2]) {
                            break;
                        }
                        k += 1;
                    }
                    let run = k - start;
                    if run > limit {
                        continue;
                    }
                    // Only a run with removed voxels at both ends is embedded.
                    let before = if start > lo[axis] {
                        c[axis] = start - 1;
                        mask.remove[(c[2] * ny + c[1]) * nx + c[0]] != 0
                    } else {
                        false
                    };
                    let after = if k <= hi[axis] {
                        c[axis] = k;
                        mask.remove[(c[2] * ny + c[1]) * nx + c[0]] != 0
                    } else {
                        false
                    };
                    if before && after {
                        for v in start..k {
                            c[axis] = v;
                            thin[((c[2] - z0) * ly + (c[1] - y0)) * lx + (c[0] - x0)] = 1;
                        }
                    }
                }
            }
        }
    }

    let mut dropped = 0usize;
    for (lz_i, z) in (z0..=z1).enumerate() {
        for (ly_i, y) in (y0..=y1).enumerate() {
            let row = (z * ny + y) * nx;
            let lrow = lz_i * lxy + ly_i * lx;
            for lx_i in 0..lx {
                if thin[lrow + lx_i] == 0 {
                    continue;
                }
                let x = x0 + lx_i;
                let cell = (z / stride[2]).min(cd[2] - 1) * cd[1] * cd[0]
                    + (y / stride[1]).min(cd[1] - 1) * cd[0]
                    + (x / stride[0]).min(cd[0] - 1);
                if core[cell] != 0 {
                    continue;
                }
                mask.remove[row + x] = 1;
                dropped += 1;
            }
        }
    }
    dropped
}

/// Drop tiny disconnected removal islands (e.g. a few noisy voxels) so the
/// defaced area stays visually continuous. The largest component is always kept,
/// as are components of at least `min_size` voxels. Only `bbox`, the bounding box
/// of the removal, is scanned.
fn drop_small_islands(mask: &mut Mask, min_size: usize, bbox: [usize; 6]) -> usize {
    let dims = mask.dims;
    let n = mask.remove.len();
    if n == 0 || bbox[0] > bbox[1] {
        return 0;
    }
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let nxy = nx * ny;
    let mut seen = vec![0u8; n];
    let mut comps: Vec<Vec<usize>> = Vec::new();
    let mut queue: VecDeque<usize> = VecDeque::new();
    for z in bbox[4]..=bbox[5] {
        for y in bbox[2]..=bbox[3] {
            let row = (z * ny + y) * nx;
            for x in bbox[0]..=bbox[1] {
                let start = row + x;
                if mask.remove[start] == 0 || seen[start] != 0 {
                    continue;
                }
                queue.clear();
                queue.push_back(start);
                seen[start] = 1;
                let mut members = Vec::new();
                while let Some(i) = queue.pop_front() {
                    members.push(i);
                    let z = i / nxy;
                    let rem = i % nxy;
                    let y = rem / nx;
                    let x = rem % nx;
                    let mut visit =
                        |xx: isize, yy: isize, zz: isize, queue: &mut VecDeque<usize>| {
                            if xx < 0 || yy < 0 || zz < 0 {
                                return;
                            }
                            let (xx, yy, zz) = (xx as usize, yy as usize, zz as usize);
                            if xx >= nx || yy >= ny || zz >= nz {
                                return;
                            }
                            let j = (zz * ny + yy) * nx + xx;
                            if mask.remove[j] != 0 && seen[j] == 0 {
                                seen[j] = 1;
                                queue.push_back(j);
                            }
                        };
                    // 26-connectivity: diagonally touching voxels are visually
                    // continuous, so they belong to the same removed area.
                    for dz in -1isize..=1 {
                        for dy in -1isize..=1 {
                            for dx in -1isize..=1 {
                                if dx == 0 && dy == 0 && dz == 0 {
                                    continue;
                                }
                                visit(
                                    x as isize + dx,
                                    y as isize + dy,
                                    z as isize + dz,
                                    &mut queue,
                                );
                            }
                        }
                    }
                }
                comps.push(members);
            }
        }
    }
    if comps.len() <= 1 {
        return 0;
    }
    let largest = comps
        .iter()
        .enumerate()
        .max_by_key(|(_, m)| m.len())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut dropped = 0usize;
    for (i, members) in comps.iter().enumerate() {
        if i == largest || members.len() >= min_size {
            continue;
        }
        for &j in members {
            mask.remove[j] = 0;
            dropped += 1;
        }
    }
    dropped
}

/// Fallback start for the vertex bone strip (columns where the per-column brain
/// surface cannot be detected): the ear-region top.
fn vertex_start_z(volume: &Volume, seg: &Segmentation) -> Option<f64> {
    let (lo, hi) = patient_bbox(volume, &seg.brain, seg.dims, seg.stride)?;
    let z_mid = 0.5 * (lo[2] + hi[2]);
    let z_half = (0.5 * (hi[2] - lo[2])).max(1.0);
    Some(z_mid + 0.55 * z_half)
}

/// Per-column patient Z of the brain's superior surface, used by the vertex
/// strip so its boundary follows the brain rather than a horizontal plane.
///
/// On CT the skull is bright: scanning down a column, the first soft tissue
/// below a bone run is the brain/CSF surface. Columns without a clear bone/brain
/// transition fall back to the topmost segmented cavity/brain cell. `NaN` means
/// no brain in that column.
fn brain_top_columns(
    volume: &Volume,
    seg: &Segmentation,
    threshold: f32,
    bone_hu: f32,
) -> Vec<f64> {
    let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
    let nxy = nx * ny;
    let mut tops = vec![f64::NAN; nxy];
    let mut bone = vec![0u8; nxy];
    let mut open = vec![1u8; nxy];
    let (pz_base, pz_step) = volume.world_axis_linear(2);
    // Walk the slices from the top down once, in slice order, resolving every
    // column as soon as its bone/soft transition is found. This keeps the memory
    // access sequential; a per-column walk down Z strides through the volume and
    // misses the cache on every voxel.
    for z in (0..nz).rev() {
        let (slope, intercept) = volume.rescale[z];
        let base = z * nxy;
        let pz_z = pz_base + pz_step[2] * z as f64;
        let mut any_open = false;
        for i in 0..nxy {
            if open[i] == 0 {
                continue;
            }
            any_open = true;
            let v = volume.data[base + i] * slope + intercept;
            if v >= bone_hu {
                bone[i] = 1;
            } else if bone[i] != 0 && v > threshold {
                let (x, y) = (i % nx, i / nx);
                tops[i] = pz_z + pz_step[0] * x as f64 + pz_step[1] * y as f64;
                open[i] = 0;
            }
        }
        if !any_open {
            break;
        }
    }
    // Fallback (e.g. MR, or a column without a clean bone run): use the topmost
    // segmented brain/cavity cell. That top is constant over a coarse cell, so
    // resolve it once per cell instead of once per voxel column.
    if open.iter().any(|&o| o != 0) {
        let (sx, sy, sz) = (seg.stride[0], seg.stride[1], seg.stride[2]);
        let (cdx, cdy, cdz) = (seg.dims[0], seg.dims[1], seg.dims[2]);
        let mut coarse_top = vec![f64::NAN; cdx * cdy];
        for cy in 0..cdy {
            for cx in 0..cdx {
                for cz in (0..cdz).rev() {
                    if seg.cavity_at(cx, cy, cz) || seg.brain_at(cx, cy, cz) {
                        let z = (cz * sz + sz - 1).min(nz - 1);
                        coarse_top[cy * cdx + cx] = pz_base + pz_step[2] * z as f64;
                        break;
                    }
                }
            }
        }
        for y in 0..ny {
            let cy = (y / sy).min(cdy - 1);
            let py = pz_step[1] * y as f64;
            for x in 0..nx {
                let i = y * nx + x;
                if open[i] == 0 {
                    continue;
                }
                let cx = (x / sx).min(cdx - 1);
                let base = coarse_top[cy * cdx + cx];
                if base.is_finite() {
                    tops[i] = base + pz_step[0] * x as f64 + py;
                }
            }
        }
    }
    tops
}

/// Patient-space bounding box `(min, max)` of a non-empty coarse `grid`.
fn patient_bbox(
    volume: &Volume,
    grid: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
) -> Option<([f64; 3], [f64; 3])> {
    if grid.is_empty() {
        return None;
    }
    let (vx, vy, vz) = coarse_axis_voxels(cd, stride, volume.dims);
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for cz in 0..cd[2] {
        for cy in 0..cd[1] {
            let row = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                if grid[row + cx] == 0 {
                    continue;
                }
                let p = volume.world(vx[cx], vy[cy], vz[cz]);
                for a in 0..3 {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
        }
    }
    (lo[0] <= hi[0]).then_some((lo, hi))
}

#[inline]
fn coarse_to_voxel(
    i: usize,
    cd: [usize; 3],
    stride: [usize; 3],
    dims: [usize; 3],
) -> (usize, usize, usize) {
    let cz = i / (cd[0] * cd[1]);
    let rem = i % (cd[0] * cd[1]);
    let cy = rem / cd[0];
    let cx = rem % cd[0];
    (
        (cx * stride[0]).min(dims[0] - 1),
        (cy * stride[1]).min(dims[1] - 1),
        (cz * stride[2]).min(dims[2] - 1),
    )
}

/// Minimum patient Y (most anterior point) of a non-empty coarse `grid`.
///
/// Uses the linear form of the patient Y coordinate so no world vector is built
/// per cell. Returns `None` for an empty/absent grid.
fn most_anterior_of_grid(
    volume: &Volume,
    grid: &[u8],
    cd: [usize; 3],
    stride: [usize; 3],
) -> Option<f64> {
    if grid.is_empty() {
        return None;
    }
    let (base, step) = volume.world_axis_linear(1);
    let (nx, ny) = (volume.nx(), volume.ny());
    let mut best = f64::MAX;
    for cz in 0..cd[2] {
        let z = (cz * stride[2]).min(volume.nz() - 1);
        let pz = base + step[2] * z as f64;
        for cy in 0..cd[1] {
            let y = (cy * stride[1]).min(ny - 1);
            let py = pz + step[1] * y as f64;
            let row = (cz * cd[1] + cy) * cd[0];
            for cx in 0..cd[0] {
                if grid[row + cx] == 0 {
                    continue;
                }
                let x = (cx * stride[0]).min(nx - 1);
                let p = py + step[0] * x as f64;
                if p < best {
                    best = p;
                }
            }
        }
    }
    (best != f64::MAX).then_some(best)
}

fn coarse_centroid(volume: &Volume, grid: &[u8], cd: [usize; 3], stride: [usize; 3]) -> V3 {
    let mut c = [0.0f64; 3];
    let mut n = 0.0f64;
    for (i, &v) in grid.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        c = add(c, volume.world(x, y, z));
        n += 1.0;
    }
    if n > 0.0 {
        scale(c, 1.0 / n)
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// 6-connected binary dilation with an L1 (diamond) structuring element of the
/// given radius.
///
/// The diamond is *not* separable into axis-aligned 1D passes (that would give a
/// cube), so this uses an exact L1 distance transform instead: a cell is
/// included when its distance to the nearest set cell is at most `radius`. Two
/// raster sweeps compute the distances, which is `O(n)` rather than the
/// `O(n * r^3)` of the direct neighbourhood scan.
fn dilate(grid: &[u8], cd: [usize; 3], radius: u32) -> Vec<u8> {
    if radius == 0 {
        return grid.to_vec();
    }
    let r = radius.min(59_999) as u16;
    l1_distance(grid, cd)
        .into_iter()
        .map(|d| u8::from(d <= r))
        .collect()
}

/// 6-connected binary erosion with an L1 (diamond) structuring element of the
/// given radius. Cells outside the grid are treated as foreground, so a closing
/// never erodes cells that merely touch the volume border.
fn erode(grid: &[u8], cd: [usize; 3], radius: u32) -> Vec<u8> {
    if radius == 0 {
        return grid.to_vec();
    }
    let r = radius.min(59_999) as u16;
    // X ⊖ B == (X^c ⊕ B)^c for the symmetric diamond B.
    let inverted: Vec<u8> = grid.iter().map(|&v| u8::from(v == 0)).collect();
    l1_distance(&inverted, cd)
        .into_iter()
        .map(|d| u8::from(d > r))
        .collect()
}

/// Separable box dilation with a per-axis radius in cells.
///
/// Used for the physical (millimetre) safety band: unlike the L1 diamond, a box
/// can have zero extent along one axis, which is what prevents a thick-slice Z
/// band from ballooning. A box structuring element is separable into three 1D
/// dilations, so this is `O(n)` per non-zero axis.
fn dilate_axes(grid: &[u8], cd: [usize; 3], radius: [usize; 3]) -> Vec<u8> {
    let mut cur = grid.to_vec();
    for axis in 0..3 {
        if radius[axis] > 0 {
            cur = dilate_axis(&cur, cd, axis, radius[axis]);
        }
    }
    cur
}

/// 1D dilation (`any` in a window) along `axis`.
fn dilate_axis(grid: &[u8], cd: [usize; 3], axis: usize, r: usize) -> Vec<u8> {
    let (n, step, lines_a, lines_b) = match axis {
        0 => (cd[0], 1usize, cd[1], cd[2]),
        1 => (cd[1], cd[0], cd[0], cd[2]),
        _ => (cd[2], cd[0] * cd[1], cd[0], cd[1]),
    };
    if n == 0 {
        return grid.to_vec();
    }
    let mut out = grid.to_vec();
    let mut line = vec![0u8; n];
    for b in 0..lines_b {
        for a in 0..lines_a {
            let base = match axis {
                0 => (b * cd[1] + a) * cd[0],
                1 => b * cd[0] * cd[1] + a,
                _ => b * cd[0] + a,
            };
            for i in 0..n {
                line[i] = grid[base + i * step];
            }
            let mut count = 0usize;
            for &v in &line[..=r.min(n - 1)] {
                count += v as usize;
            }
            for i in 0..n {
                if count > 0 {
                    out[base + i * step] = 1;
                }
                if i >= r {
                    count -= line[i - r] as usize;
                }
                let add = i + r + 1;
                if add < n {
                    count += line[add] as usize;
                }
            }
        }
    }
    out
}

/// Exact L1 (6-connected) distance from every cell to the nearest set cell.
/// Set cells are `0`; unreachable cells sit at `INF`.
fn l1_distance(grid: &[u8], cd: [usize; 3]) -> Vec<u16> {
    const INF: u16 = 60_000;
    let (nx, ny, nz) = (cd[0], cd[1], cd[2]);
    let nxy = nx * ny;
    let mut d = vec![INF; grid.len()];
    for (di, &g) in d.iter_mut().zip(grid) {
        if g != 0 {
            *di = 0;
        }
    }
    // Forward sweep (-x, -y, -z).
    for z in 0..nz {
        for y in 0..ny {
            let row = (z * ny + y) * nx;
            for x in 0..nx {
                let i = row + x;
                let mut m = d[i];
                if x > 0 && d[i - 1] < m {
                    m = d[i - 1];
                }
                if y > 0 && d[i - nx] < m {
                    m = d[i - nx];
                }
                if z > 0 && d[i - nxy] < m {
                    m = d[i - nxy];
                }
                let candidate = m + 1;
                if candidate < d[i] {
                    d[i] = candidate;
                }
            }
        }
    }
    // Backward sweep (+x, +y, +z).
    for z in (0..nz).rev() {
        for y in (0..ny).rev() {
            let row = (z * ny + y) * nx;
            for x in (0..nx).rev() {
                let i = row + x;
                let mut m = d[i];
                if x + 1 < nx && d[i + 1] < m {
                    m = d[i + 1];
                }
                if y + 1 < ny && d[i + nx] < m {
                    m = d[i + nx];
                }
                if z + 1 < nz && d[i + nxy] < m {
                    m = d[i + nxy];
                }
                let candidate = m + 1;
                if candidate < d[i] {
                    d[i] = candidate;
                }
            }
        }
    }
    d
}

/// Map the coarse head/brain masks back to the full-resolution volume grid.
pub fn masks_to_volume(seg: &Segmentation, volume: &Volume) -> (Mask, Mask) {
    let mut head = Mask::new(volume.dims);
    let mut brain = Mask::new(volume.dims);
    // By construction each coarse block is `stride` wide; fill whole blocks.
    for cz in 0..seg.dims[2] {
        for cy in 0..seg.dims[1] {
            for cx in 0..seg.dims[0] {
                let ci = seg.cidx(cx, cy, cz);
                let is_head = seg.head[ci] != 0;
                let is_brain = seg.brain[ci] != 0;
                if !is_head && !is_brain {
                    continue;
                }
                for dz in 0..seg.stride[2] {
                    let z = cz * seg.stride[2] + dz;
                    if z >= volume.nz() {
                        break;
                    }
                    for dy in 0..seg.stride[1] {
                        let y = cy * seg.stride[1] + dy;
                        if y >= volume.ny() {
                            break;
                        }
                        for dx in 0..seg.stride[0] {
                            let x = cx * seg.stride[0] + dx;
                            if x >= volume.nx() {
                                break;
                            }
                            if is_head {
                                head.set(x, y, z, true);
                            }
                            if is_brain {
                                brain.set(x, y, z, true);
                            }
                        }
                    }
                }
            }
        }
    }
    (head, brain)
}

/// Build a full-resolution [`Mask`] of the **protected intracranial core**:
/// the segmented cavity ∪ vault (plus the brain if there is no cavity/vault),
/// padded by a physical `protect_mm` safety band so the cut keeps clear of the
/// brain and vault. The padding is per-axis in millimetres, so thick CT slices do
/// not turn a 2 mm margin into a whole-slice band.
///
/// A defacing mask from *any* backend can be made brain-safe by clearing the
/// voxels set here. Returns `Ok(None)` when no brain could be segmented (so the
/// caller can decide whether to proceed).
pub fn brain_protect_mask(
    volume: &Volume,
    seg_params: &SegParams,
    protect_mm: f64,
) -> Result<Option<Mask>, String> {
    let seg = segment(volume, seg_params)?;
    if seg.brain_count() == 0 {
        return Ok(None);
    }
    let core = protected_core(
        &seg.cavity,
        &seg.vault,
        &seg.brain,
        seg.dims,
        seg.stride,
        volume.spacing,
        protect_mm,
    );

    let mut mask = Mask::new(volume.dims);
    for cz in 0..seg.dims[2] {
        for cy in 0..seg.dims[1] {
            for cx in 0..seg.dims[0] {
                if core[seg.cidx(cx, cy, cz)] == 0 {
                    continue;
                }
                for dz in 0..seg.stride[2] {
                    let z = cz * seg.stride[2] + dz;
                    if z >= volume.nz() {
                        break;
                    }
                    for dy in 0..seg.stride[1] {
                        let y = cy * seg.stride[1] + dy;
                        if y >= volume.ny() {
                            break;
                        }
                        for dx in 0..seg.stride[0] {
                            let x = cx * seg.stride[0] + dx;
                            if x >= volume.nx() {
                                break;
                            }
                            mask.set(x, y, z, true);
                        }
                    }
                }
            }
        }
    }
    Ok(Some(mask))
}

/// What the anterior cut is referenced to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutReference {
    /// Cut in front of the **brain's frontal pole** (removes the frontal face;
    /// never crosses into brain).
    BrainFront,
    /// Cut in front of the **cranial vault** (the frontal bone). Even more
    /// conservative — only removes what is anterior to the calvarium. Falls back
    /// to `BrainFront` when no vault was detected (e.g. MR).
    SkullFront,
}

impl Default for CutReference {
    fn default() -> Self {
        CutReference::BrainFront
    }
}

/// What the segmentation backend removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskRegion {
    /// Flat anterior cut in front of the brain (conservative; the face only).
    /// Set [`SegBackendParams::include_orbits`] /
    /// [`SegBackendParams::include_ears`] / [`SegBackendParams::include_zygoma`]
    /// to also remove the orbits / ears / zygoma. The superior skull cap is
    /// stripped by default (`vertex_strip`), following the per-column brain
    /// surface. Tiny isolated removal specks and thin kept slivers are pruned so
    /// the defaced area is one visually continuous region.
    Face,
    /// "Deflesh": remove external soft tissue and bone — everything outside the
    /// **cranial vault** (scalp, face, table, any tissue outside the vault).
    /// Keeps the intracranial cavity and the vault that encloses it.
    ExternalSoftTissue,
}

impl Default for MaskRegion {
    fn default() -> Self {
        MaskRegion::Face
    }
}

/// Parameters for [`SegmentationBackend`].
#[derive(Clone, Debug)]
pub struct SegBackendParams {
    /// Segmentation parameters.
    pub seg: SegParams,
    /// What the backend removes.
    pub region: MaskRegion,
    /// What the anterior cut is referenced to.
    pub cut_reference: CutReference,
    /// Extra anterior margin (mm) kept in front of the brain before cutting.
    pub brain_margin_mm: f64,
    /// How far in front of the skull to place the cut (mm), for
    /// [`CutReference::SkullFront`].
    pub skull_depth_mm: f64,
    /// Reject a mask that removes more than this fraction of all voxels.
    pub max_removed_fraction: f64,
    /// For [`MaskRegion::ExternalSoftTissue`]: how far **posterior** of the brain
    /// centre (mm) external tissue is still removed. External voxels posterior of
    /// `brain_centroid_y + deflesh_posterior_mm` are **kept**, so the posterior
    /// scalp, neck and back of the head are preserved and only the anterior
    /// face/scalp is stripped. Set to `f64::INFINITY` to remove all external
    /// tissue (the old behaviour).
    pub deflesh_posterior_mm: f64,
    /// Safety band (mm) added around the protected intracranial core (the
    /// `cavity ∪ vault`). No voxel within this distance of the core is removed by
    /// **either** region, so the cut can never extend into the brain or vault
    /// (except for the explicit vertex strip, which removes superior vault bone
    /// while still guarding the brain/CSF). Larger values are safer (and remove
    /// slightly less face); `0` disables the extra band. Default `2.0`.
    pub brain_protect_mm: f64,
    /// Feather width (mm) for [`MaskRegion::ExternalSoftTissue`] ("deflesh").
    /// Instead of a hard binary wall at the keep boundary, removed voxels within
    /// this distance of the boundary are blended toward the fill by a weight that
    /// ramps from `0` at the boundary to `1` this far inside — so the defleshed
    /// surface fades smoothly rather than ending in a flat cut. `0` disables the
    /// feather (hard cut); the default `3.0` gives a soft edge.
    pub deflesh_smooth_mm: f64,
    /// Also remove the orbital contents (the eyes/globes) for
    /// [`MaskRegion::Face`].
    /// The flat face cut sits in front of the brain; depending on the margin and
    /// the subject's anatomy the globes can survive it. When enabled, an
    /// anterior, orbit-level region derived from the segmented brain is removed
    /// as well, so the eyes are always included. The region stops in front of the
    /// brain's anterior pole (with a small buffer), so it cannot reach the
    /// inferior frontal lobes or the temporal fossa, and it is guarded by the
    /// segmented **brain** and by actual **bone** voxels. That guard (rather than
    /// the intracranial cavity or the configurable safety band) is essential: the
    /// cavity grow can leak through the optic canal into an orbit, and a coarse
    /// vault cell can contain the globe next to the orbital rim; neither may keep
    /// the globe. The orbit region itself does not remove brain or vault bone;
    /// the separate vertex strip may remove superior vault bone.
    ///
    /// Has no effect for [`MaskRegion::ExternalSoftTissue`], which already
    /// removes everything outside the cranial vault.
    pub include_orbits: bool,
    /// Also remove the ears for [`MaskRegion::Face`].
    ///
    /// A flat face cut leaves the auricles (and the temporal scalp at their
    /// level), which are a strong facial-recognition cue. When enabled, external
    /// tissue outside the protected core is removed in an ear-height band from
    /// the face back past the full auricle. The occiput, nape and posterior
    /// scalp are kept, and the brain/vault core is never touched.
    ///
    /// Has no effect for [`MaskRegion::ExternalSoftTissue`], which already
    /// removes the ears.
    pub include_ears: bool,
    /// Also remove the zygoma (the cheekbone and zygomatic arch) and the lateral
    /// skull base behind it, for [`MaskRegion::Face`].
    ///
    /// The zygomatic bone and arch sit lateral to the maxilla and orbit and
    /// *behind* the flat cut plane, so a plane cut leaves the cheek prominence —
    /// a strong facial-recognition cue — in place. When enabled, a lateral region
    /// below the orbital floor is removed back past the temporomandibular joint
    /// into the lateral skull base: bone and soft tissue alike, since the arch is
    /// bone that the vault guard would otherwise keep. Only the segmented brain
    /// and CSF are protected, with a wider band than the other regions (8 mm,
    /// because the coarse brain grid can undershoot the lowest temporal gyri), and
    /// the region stops below the orbital floor. The skull base under the inferior
    /// temporal lobes and the temporal lobe itself are therefore never touched.
    ///
    /// Has no effect for [`MaskRegion::ExternalSoftTissue`].
    pub include_zygoma: bool,
    /// For [`MaskRegion::Face`], strip the skull cap toward the vertex
    /// (default `true`).
    ///
    /// The boundary follows the brain's superior surface per column (detected
    /// from the bone/soft-tissue transition), so it removes the superior vault
    /// bone and overlying scalp without cutting a straight slice through the
    /// skull. Soft brain/CSF inside the guarded core is kept; bone in a coarse
    /// brain cell is stripped. Set `false` to keep the vertex intact.
    pub vertex_strip: bool,
}

impl Default for SegBackendParams {
    fn default() -> Self {
        SegBackendParams {
            seg: SegParams::default(),
            region: MaskRegion::default(),
            cut_reference: CutReference::BrainFront,
            brain_margin_mm: 10.0,
            skull_depth_mm: 45.0,
            max_removed_fraction: 0.6,
            deflesh_posterior_mm: 0.0,
            brain_protect_mm: 2.0,
            deflesh_smooth_mm: 3.0,
            include_orbits: false,
            include_ears: false,
            include_zygoma: false,
            vertex_strip: true,
        }
    }
}

/// In-house segmentation defacing backend: segment head+brain, then remove the
/// foreground anterior to the brain's front boundary (with a safety margin).
///
/// Unlike the atlas backend there is no template file — the brain boundary is
/// estimated directly from the image.
pub struct SegmentationBackend {
    pub params: SegBackendParams,
}

impl SegmentationBackend {
    pub fn new(params: SegBackendParams) -> Self {
        SegmentationBackend { params }
    }
}

impl crate::backend::DefacingBackend for SegmentationBackend {
    fn name(&self) -> &str {
        "segmentation"
    }

    fn description(&self) -> String {
        "In-house head/brain segmentation; cut in front of the brain".to_string()
    }

    fn compute_mask(&self, volume: &Volume) -> Result<Mask, String> {
        let seg = segment(volume, &self.params.seg)?;
        self.compute_mask_from_segmentation(volume, &seg)
    }
}

impl SegmentationBackend {
    /// Build the removal mask from an already-computed [`Segmentation`].
    ///
    /// The segmentation (threshold + morphology + brain/vault/cavity grow) is
    /// the dominant cost of this backend and does not depend on the defacing
    /// parameters that only move the cut plane (margin, region, safety band).
    /// Callers that tune those parameters live — e.g. the viewer preview — can
    /// segment once and reuse it across slider changes.
    pub fn compute_mask_from_segmentation(
        &self,
        volume: &Volume,
        seg: &Segmentation,
    ) -> Result<Mask, String> {
        if volume.is_empty() {
            return Err("cannot deface an empty volume".to_string());
        }
        if seg.brain_count() == 0 {
            return Err("segmentation found no brain region; refusing to deface".to_string());
        }
        let threshold = crate::geometric::choose_threshold(volume, &self.params.seg.threshold);

        // Reference point for the anterior cut, plus a margin.
        // - BrainFront: a plane in front of the brain's frontal pole.
        // - SkullFront: a plane in front of the **cranial vault** (the frontal
        //   bone over the frontal lobes), which removes the whole face/sinuses
        //   while never crossing the vault. Falls back to the brain front when no
        //   vault exists (e.g. MR).
        let most_anterior_brain = most_anterior_of_grid(volume, &seg.brain, seg.dims, seg.stride)
            .ok_or_else(|| {
            "segmentation found no brain region; refusing to deface".to_string()
        })?;

        // Most-anterior vault point (patient Y) — the frontal bone.
        let most_anterior_vault =
            most_anterior_of_grid(volume, &seg.vault, seg.dims, seg.stride).unwrap_or(f64::MAX);

        // Anterior cut plane: remove everything with Y anterior of it.
        // - BrainFront: `margin` in front of the frontal pole (smaller margin
        //   removes more face; larger is safer).
        // - SkullFront: `margin` in front of the vault's frontal bone, so the cut
        //   never crosses the vault; falls back to the brain front without a vault.
        let margin = self.params.brain_margin_mm;
        let reference_y = if self.params.cut_reference == CutReference::SkullFront
            && most_anterior_vault != f64::MAX
            && most_anterior_vault < most_anterior_brain
        {
            // The vault is anterior to the brain (as it should be when the skull
            // is present): cut in front of it.
            most_anterior_vault
        } else {
            most_anterior_brain
        };
        let cut_y = reference_y - margin;

        // Protected core: nothing within `brain_protect_mm` of the intracranial
        // contents is ever removed, by either region. This is a hard guarantee
        // that the cut cannot extend into brain or vault, even though the cut
        // plane is computed on a coarse grid.
        let core = protected_core(
            &seg.cavity,
            &seg.vault,
            &seg.brain,
            seg.dims,
            seg.stride,
            volume.spacing,
            self.params.brain_protect_mm,
        );
        let fill_guard = strict_core_guard(seg);
        let sliver_guard = strict_core_and_cavity_guard(seg);

        let mut mask = Mask::new(volume.dims);
        let mut removed = 0usize;

        if self.params.region == MaskRegion::ExternalSoftTissue {
            // "Deflesh": remove external soft tissue and bone — everything
            // **outside the cranial vault** — while keeping the intracranial
            // contents (brain, CSF) and the vault that encloses them. The keep
            // boundary is the protected core (`cavity ∪ vault` + safety band), so
            // it stops at the outer table of the calvarium and never reaches into
            // the face (facial bones are not part of the vault) or the brain.
            let keep = &core;

            // Only strip the anterior face/scalp by default: keep external tissue
            // posterior of `brain_centroid_y + deflesh_posterior_mm` (the posterior
            // scalp, neck and back of the head). With `INFINITY` this gate is
            // inactive and all external tissue is removed.
            let posterior_limit = seg.brain_centroid[1] + self.params.deflesh_posterior_mm;

            // Voxel -> coarse-cell maps, so the per-voxel loop needs no division.
            let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
            let cx_of: Vec<usize> = (0..nx)
                .map(|x| (x / seg.stride[0]).min(seg.dims[0] - 1))
                .collect();
            let cy_of: Vec<usize> = (0..ny)
                .map(|y| (y / seg.stride[1]).min(seg.dims[1] - 1))
                .collect();
            let cz_of: Vec<usize> = (0..nz)
                .map(|z| (z / seg.stride[2]).min(seg.dims[2] - 1))
                .collect();
            // Patient Y is linear in the voxel indices; hoist the per-slice and
            // per-row terms instead of building a world point per voxel.
            let (py_base, py_step) = volume.world_axis_linear(1);
            let nxy = nx * ny;
            // Bounding box of removed voxels, so the feather pass can skip the
            // bulk of the volume when seeding its BFS.
            let mut bbox = [usize::MAX, 0, usize::MAX, 0, usize::MAX, 0];

            for z in 0..nz {
                let (slope, intercept) = volume.rescale[z];
                let row_z = z * nxy;
                let cz = cz_of[z];
                let py_z = py_base + py_step[2] * z as f64;
                for y in 0..ny {
                    let row = row_z + y * nx;
                    let core_row = (cz * seg.dims[1] + cy_of[y]) * seg.dims[0];
                    let py_y = py_z + py_step[1] * y as f64;
                    for x in 0..nx {
                        let i = row + x;
                        if volume.data[i] * slope + intercept <= threshold {
                            continue;
                        }
                        if keep[core_row + cx_of[x]] != 0 {
                            continue;
                        }
                        if py_y + py_step[0] * x as f64 >= posterior_limit {
                            continue; // posterior external tissue is kept (neck)
                        }
                        mask.remove[i] = 1;
                        removed += 1;
                        bbox = [
                            bbox[0].min(x),
                            bbox[1].max(x),
                            bbox[2].min(y),
                            bbox[3].max(y),
                            bbox[4].min(z),
                            bbox[5].max(z),
                        ];
                    }
                }
            }

            // Keep the defaced area one clean region: drop tiny isolated removal
            // specks, drop thin kept slivers left by a region boundary, then fill
            // the small kept islands those passes expose.
            let pruned = drop_small_islands(&mut mask, 256, bbox);
            removed = removed.saturating_sub(pruned);
            removed += remove_thin_kept_runs(
                &mut mask,
                volume,
                threshold,
                &sliver_guard,
                seg.dims,
                seg.stride,
                bbox,
                THIN_KEPT_MM,
            );
            let filled = fill_small_kept_islands(
                &mut mask,
                volume,
                threshold,
                &fill_guard,
                seg.dims,
                seg.stride,
                bbox,
                1024,
            );
            removed += filled;

            // Optional feather: ramp the removal weight from 0 at the kept
            // boundary to 1 `deflesh_smooth_mm` inside, so the surface fades
            // instead of ending in a hard wall. Only the boundary voxels change;
            // the binary `remove` set (and the safety guarantee) is unaffected.
            // Runs after the continuity passes so their new boundary is feathered
            // too.
            if self.params.deflesh_smooth_mm > 0.0 && removed > 0 {
                feather_boundary(
                    &mut mask,
                    volume,
                    self.params.deflesh_smooth_mm,
                    &core,
                    seg.dims,
                    seg.stride,
                    bbox,
                );
            }

            let max_removed = (self.params.max_removed_fraction * volume.len() as f64) as usize;
            if removed > max_removed {
                return Err(format!(
                    "segmentation backend would remove {} of {} voxels (> {:.0}%); refusing",
                    removed,
                    volume.len(),
                    self.params.max_removed_fraction * 100.0
                ));
            }
            return Ok(mask);
        }

        // "Face": remove everything anterior of the cut plane. Same flat walk as
        // above: the rescale is hoisted per slice and patient Y comes from its
        // linear form.
        //
        // The optional orbit region is shallow (it ends just behind the brain's
        // anterior pole) and is guarded by the segmented brain and by actual bone
        // voxels, not by the intracranial cavity or the configurable safety band.
        // The cavity grow can leak through the optic canal into an orbit, and a
        // coarse vault cell can contain the globe next to the orbital rim; neither
        // may keep the globe.
        //
        // The optional ear region removes external tissue at ear height outside
        // the protected core, back to roughly the brain centroid. The occiput and
        // nape are kept, and the brain/vault core is never touched.
        let orbit = if self.params.include_orbits {
            orbit_region(volume, seg)
        } else {
            None
        };
        let ear = if self.params.include_ears {
            ear_region(volume, seg)
        } else {
            None
        };
        let zygoma = if self.params.include_zygoma {
            zygoma_region(volume, seg)
        } else {
            None
        };
        let anterior_limit = orbit
            .as_ref()
            .map(|r| r.y_max)
            .unwrap_or(f64::MIN)
            .max(ear.as_ref().map(|r| r.y_max).unwrap_or(f64::MIN))
            .max(zygoma.as_ref().map(|r| r.y_max).unwrap_or(f64::MIN));
        let vertex_z = if self.params.vertex_strip {
            vertex_start_z(volume, seg)
        } else {
            None
        };
        let vertex_tops = if self.params.vertex_strip {
            Some(brain_top_columns(
                volume,
                seg,
                threshold,
                self.params.seg.bone_hu,
            ))
        } else {
            None
        };
        // The zygoma region reaches the skull base right under the temporal lobe,
        // so it keeps a wider band around the brain/CSF than the other regions:
        // the coarse brain grid can undershoot the lowest temporal gyri.
        const ZYGOMA_BRAIN_BAND_MM: f64 = 8.0;
        let zygoma_guard = zygoma.map(|_| {
            protected_core(
                &seg.cavity,
                &[],
                &seg.brain,
                seg.dims,
                seg.stride,
                volume.spacing,
                self.params.brain_protect_mm.max(ZYGOMA_BRAIN_BAND_MM),
            )
        });
        let vertex_margin = self.params.brain_protect_mm.max(2.0);
        let vertex_guard = vertex_z.map(|_| {
            protected_core(
                &seg.cavity,
                &[],
                &seg.brain,
                seg.dims,
                seg.stride,
                volume.spacing,
                self.params.brain_protect_mm.max(2.0),
            )
        });
        let (nx, ny, nz) = (volume.nx(), volume.ny(), volume.nz());
        let cx_of: Vec<usize> = (0..nx)
            .map(|x| (x / seg.stride[0]).min(seg.dims[0] - 1))
            .collect();
        let cy_of: Vec<usize> = (0..ny)
            .map(|y| (y / seg.stride[1]).min(seg.dims[1] - 1))
            .collect();
        let cz_of: Vec<usize> = (0..nz)
            .map(|z| (z / seg.stride[2]).min(seg.dims[2] - 1))
            .collect();
        let (py_base, py_step) = volume.world_axis_linear(1);
        let (pz_base, pz_step) = volume.world_axis_linear(2);
        let (px_base, px_step) = volume.world_axis_linear(0);
        let nxy = nx * ny;

        for z in 0..nz {
            let (slope, intercept) = volume.rescale[z];
            let row_z = z * nxy;
            let cz = cz_of[z];
            let py_z = py_base + py_step[2] * z as f64;
            let pz_z = pz_base + pz_step[2] * z as f64;
            let px_z = px_base + px_step[2] * z as f64;
            for y in 0..ny {
                let row = row_z + y * nx;
                let cy = cy_of[y];
                let core_row = (cz * seg.dims[1] + cy) * seg.dims[0];
                let py_y = py_z + py_step[1] * y as f64;
                let pz_y = pz_z + pz_step[1] * y as f64;
                let px_y = px_z + px_step[1] * y as f64;
                for x in 0..nx {
                    let i = row + x;
                    let v = volume.data[i] * slope + intercept;
                    if v <= threshold {
                        continue;
                    }
                    let py = py_y + py_step[0] * x as f64;
                    let pz = pz_y + pz_step[0] * x as f64;
                    // The orbit/ear regions can overlap the flat face slab; when
                    // they do, they still use their own guards below.
                    let mut in_orbit = false;
                    let mut in_ear = false;
                    let mut in_zygoma = false;
                    let in_vertex = match (&vertex_tops, vertex_z) {
                        (Some(tops), fallback) => {
                            let top = tops[y * nx + x];
                            if top.is_finite() {
                                pz > top + vertex_margin
                            } else {
                                fallback.map_or(false, |z| pz >= z)
                            }
                        }
                        _ => false,
                    };
                    if !in_vertex && py < anterior_limit {
                        let px = px_y + px_step[0] * x as f64;
                        if let Some(region) = &orbit {
                            if py < region.y_max && pz >= region.z[0] && pz <= region.z[1] {
                                in_orbit = region.contains([px, py, pz]);
                            }
                        }
                        if !in_orbit {
                            if let Some(region) = &ear {
                                in_ear = region.contains([px, py, pz]);
                            }
                        }
                        if !in_orbit && !in_ear {
                            if let Some(region) = &zygoma {
                                in_zygoma = region.contains([px, py, pz]);
                            }
                        }
                    }
                    if in_vertex {
                        // Strip the superior vault/scalp towards the vertex. Where
                        // the per-column brain surface was detected, the height
                        // test (with `vertex_margin`) is the guard; the coarse
                        // fallback still uses the segmented core guard.
                        let top = vertex_tops
                            .as_ref()
                            .map(|tops| tops[y * nx + x])
                            .unwrap_or(f64::NAN);
                        let guard = vertex_guard.as_ref().unwrap();
                        let protected = guard[core_row + cx_of[x]] != 0;
                        if top.is_finite() {
                            // Above the detected brain surface: always strip bone;
                            // keep only non-bone tissue inside the coarse brain
                            // guard, so the brain cannot be cut.
                            if !(v >= self.params.seg.bone_hu) && protected {
                                continue;
                            }
                        } else if protected {
                            continue;
                        }
                    } else if in_orbit {
                        // Keep segmented brain (including one cell posterior, to
                        // catch a missed brain edge) and actual vault bone. Everything
                        // else here is orbital content: remove it even if a coarse
                        // vault cell contains bone, or the cavity leaked into the
                        // orbit.
                        let cx = cx_of[x];
                        let brain = seg.brain_at(cx, cy, cz)
                            || (cy + 1 < seg.dims[1] && seg.brain_at(cx, cy + 1, cz));
                        if brain {
                            continue;
                        }
                        if v >= self.params.seg.bone_hu && seg.vault_at(cx, cy, cz) {
                            continue;
                        }
                    } else if in_ear {
                        // Ear-height external tissue: remove everything outside
                        // the protected core (brain/vault + safety band).
                        if core[core_row + cx_of[x]] != 0 {
                            continue;
                        }
                    } else if in_zygoma {
                        // Cheekbone, zygomatic arch and the lateral skull base
                        // behind them. They are bone, so the vault guard must not
                        // apply here: only the brain/CSF (with the safety band)
                        // is protected, which keeps the temporal lobe intact.
                        let guard = zygoma_guard.as_ref().unwrap();
                        if guard[core_row + cx_of[x]] != 0 {
                            continue;
                        }
                    } else {
                        if py >= cut_y {
                            continue;
                        }
                        // Hard guard: never remove anything in the protected core
                        // (brain/vault + safety band), even if the coarse plane
                        // says so.
                        if core[core_row + cx_of[x]] != 0 {
                            continue;
                        }
                    }
                    mask.remove[i] = 1;
                    removed += 1;
                }
            }
        }

        // Keep the defaced area one clean region: drop tiny isolated removal
        // specks, drop thin kept slivers left by a region boundary, then fill
        // the small kept islands those passes expose.
        let bbox = match removal_bbox(&mask) {
            Some(b) => b,
            None => return Ok(mask),
        };
        let pruned = drop_small_islands(&mut mask, 256, bbox);
        removed = removed.saturating_sub(pruned);
        removed += remove_thin_kept_runs(
            &mut mask,
            volume,
            threshold,
            &sliver_guard,
            seg.dims,
            seg.stride,
            bbox,
            THIN_KEPT_MM,
        );
        let filled = fill_small_kept_islands(
            &mut mask,
            volume,
            threshold,
            &fill_guard,
            seg.dims,
            seg.stride,
            bbox,
            1024,
        );
        removed += filled;

        let max_removed = (self.params.max_removed_fraction * volume.len() as f64) as usize;
        if removed > max_removed {
            return Err(format!(
                "segmentation backend would remove {} of {} voxels (> {:.0}%); refusing",
                removed,
                volume.len(),
                self.params.max_removed_fraction * 100.0
            ));
        }
        Ok(mask)
    }
}

/// Build a closure testing whether a full-resolution voxel is inside the
/// segmented head. Used to stabilise `head_statistics` on wide-FOV scans.
pub fn head_filter<'a>(
    seg: &'a Segmentation,
    _volume: &'a Volume,
) -> impl Fn(usize, usize, usize) -> bool + 'a {
    move |x, y, z| {
        let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
        let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
        let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
        seg.head_at(cx, cy, cz)
    }
}

/// Front (anterior) boundary of the brain, for cutting the face without
/// touching brain. Returns the patient-space point on the brain's anterior
/// surface nearest the given superior/left coordinates.
pub fn brain_front(seg: &Segmentation, volume: &Volume) -> Option<V3> {
    let mut best: Option<(f64, V3)> = None;
    for (i, &b) in seg.brain.iter().enumerate() {
        if b == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, seg.dims, seg.stride, volume.dims);
        let p = volume.world(x, y, z);
        // Most anterior = smallest Y.
        if best.as_ref().map(|(y0, _)| p[1] < *y0).unwrap_or(true) {
            best = Some((p[1], p));
        }
    }
    best.map(|(_, p)| p)
}

/// Unit vector from the brain centroid toward anterior, using the estimated head
/// frame. Falls back to patient -Y.
pub fn brain_anterior_axis(stats_anterior: V3) -> V3 {
    normalize(stats_anterior).unwrap_or([0.0, -1.0, 0.0])
}

#[cfg(test)]
mod morph_tests {
    use super::{dilate, erode};

    /// Straightforward per-cell reference implementation of the 6-connected
    /// (L1/diamond) morphology the distance-transform passes must reproduce.
    fn naive(grid: &[u8], cd: [usize; 3], radius: u32, is_erode: bool) -> Vec<u8> {
        let r = radius as isize;
        let mut out = grid.to_vec();
        for z in 0..cd[2] {
            for y in 0..cd[1] {
                for x in 0..cd[0] {
                    let mut any = false;
                    let mut all = true;
                    for dz in -r..=r {
                        for dy in -r..=r {
                            for dx in -r..=r {
                                if dx.abs() + dy.abs() + dz.abs() > r {
                                    continue;
                                }
                                let (nx, ny, nz) =
                                    (x as isize + dx, y as isize + dy, z as isize + dz);
                                if nx < 0 || ny < 0 || nz < 0 {
                                    continue;
                                }
                                let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                                if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                                    continue;
                                }
                                if grid[(nz * cd[1] + ny) * cd[0] + nx] != 0 {
                                    any = true;
                                } else {
                                    all = false;
                                }
                            }
                        }
                    }
                    out[(z * cd[1] + y) * cd[0] + x] = if is_erode { all } else { any } as u8;
                }
            }
        }
        out
    }

    fn pseudo_random_grid(cd: [usize; 3], seed: u32) -> Vec<u8> {
        let mut state = seed | 1;
        (0..cd[0] * cd[1] * cd[2])
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                u8::from(state >> 30 == 0)
            })
            .collect()
    }

    #[test]
    fn distance_transform_morphology_matches_naive_reference() {
        let cd = [9, 8, 7];
        for seed in 0..4u32 {
            let grid = pseudo_random_grid(cd, seed + 1);
            for radius in 0..=3u32 {
                assert_eq!(
                    dilate(&grid, cd, radius),
                    naive(&grid, cd, radius, false),
                    "dilate r={radius} seed={seed}"
                );
                assert_eq!(
                    erode(&grid, cd, radius),
                    naive(&grid, cd, radius, true),
                    "erode r={radius} seed={seed}"
                );
            }
        }
    }

    #[test]
    fn dilation_grows_all_six_neighbours() {
        let cd = [5, 5, 5];
        let mut grid = vec![0u8; 125];
        grid[(2 * 5 + 2) * 5 + 2] = 1;
        let out = dilate(&grid, cd, 1);
        assert_eq!(out.iter().filter(|&&v| v != 0).count(), 7);
    }

    #[test]
    fn thin_kept_sheets_inside_the_removal_are_dropped() {
        use super::{removal_bbox, remove_thin_kept_runs, Mask, Volume};

        // 10^3 volume, all soft tissue; the removal is the block x=2..7 with a
        // one-voxel kept sheet through it and a thick kept column beside it.
        let (nx, ny, nz) = (10usize, 10usize, 10usize);
        let dims = [nx, ny, nz];
        let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
        let data = vec![100.0f32; nx * ny * nz];
        let volume = Volume {
            data,
            dims,
            spacing: [1.0, 1.0, 1.0],
            origin: [0.0, 0.0, 0.0],
            dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            rescale: vec![(1.0, 0.0); nz],
            modality: "CT".to_string(),
        };
        let mut mask = Mask::new(dims);
        for z in 1..9 {
            for y in 1..9 {
                for x in 1..9 {
                    mask.remove[idx(x, y, z)] = 1;
                }
            }
        }
        // A one-voxel sheet through the removal (kept at y=5).
        for z in 1..9 {
            for x in 1..9 {
                mask.remove[idx(x, 5, z)] = 0;
            }
        }
        // A thick kept column (4 voxels wide) through it: not a sheet.
        for z in 1..9 {
            for x in 1..5 {
                for y in 4..8 {
                    mask.remove[idx(x, y, z)] = 0;
                }
            }
        }
        let cd = [3, 3, 3];
        let stride = [3, 3, 3];
        let core = vec![0u8; cd[0] * cd[1] * cd[2]];
        let bbox = removal_bbox(&mask).expect("bbox");
        let dropped = remove_thin_kept_runs(&mut mask, &volume, 0.0, &core, cd, stride, bbox, 2.0);
        assert!(dropped > 0, "the one-voxel sheet is dropped");
        assert_eq!(
            mask.remove[idx(5, 5, 4)],
            1,
            "a voxel of the thin sheet is now removed"
        );
        assert_eq!(
            mask.remove[idx(2, 5, 4)],
            0,
            "the thick kept column is left alone"
        );
        assert_eq!(
            mask.remove[idx(3, 6, 4)],
            0,
            "the thick kept column is left alone (interior)"
        );
    }

    #[test]
    fn kept_islands_bordering_the_removal_are_filled_but_guarded_ones_are_not() {
        use super::{fill_small_kept_islands, removal_bbox, Mask, Volume};
        // 10^3 volume; a 9x9x9 removal block in the middle with a few kept
        // specks inside it.
        let (nx, ny, nz) = (10usize, 10usize, 10usize);
        let dims = [nx, ny, nz];
        let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
        let mut data = vec![0.0f32; nx * ny * nz];
        for z in 2..9 {
            for y in 2..9 {
                for x in 2..9 {
                    data[idx(x, y, z)] = 100.0;
                }
            }
        }
        let volume = Volume {
            data,
            dims,
            spacing: [1.0, 1.0, 1.0],
            origin: [0.0, 0.0, 0.0],
            dir: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            rescale: vec![(1.0, 0.0); nz],
            modality: "CT".to_string(),
        };
        let mut mask = Mask::new(dims);
        for z in 2..9 {
            for y in 2..9 {
                for x in 2..9 {
                    mask.remove[idx(x, y, z)] = 1;
                }
            }
        }
        // A single-voxel island enclosed by the removal -> must be filled.
        let island = idx(4, 4, 4);
        mask.remove[island] = 0;
        // An island inside a guarded (brain/vault) coarse cell -> must be kept.
        let guarded = idx(7, 7, 7);
        mask.remove[guarded] = 0;
        // A speck in air, not bordering the removal -> must be kept.
        let floating = idx(0, 0, 0);
        mask.remove[floating] = 0;
        // A kept channel longer than `max_size` inside the removal -> kept.
        for x in 4..=8 {
            mask.remove[idx(x, 2, 5)] = 0;
        }
        let cd = [3, 3, 3];
        let stride = [3, 3, 3];
        let mut core = vec![0u8; cd[0] * cd[1] * cd[2]];
        // Cell of (7, 7, 7) is (2, 2, 2).
        core[(2 * cd[1] + 2) * cd[0] + 2] = 1;
        let Some(bbox) = removal_bbox(&mask) else {
            panic!("removal bbox");
        };
        let filled = fill_small_kept_islands(&mut mask, &volume, 0.0, &core, cd, stride, bbox, 4);
        assert_eq!(filled, 1, "only the enclosed island is filled");
        assert_eq!(mask.remove[island], 1, "enclosed island is filled");
        assert_eq!(mask.remove[guarded], 0, "guarded island is kept");
        assert_eq!(mask.remove[floating], 0, "floating speck is kept");
        for x in 4..=8 {
            assert_eq!(
                mask.remove[idx(x, 2, 5)],
                0,
                "a kept channel larger than max_size is kept"
            );
        }
    }
}
