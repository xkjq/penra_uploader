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

    // Coarse foreground occupancy (any voxel in the block above threshold).
    let mut fg = vec![0u8; cn];
    for cz in 0..cd[2] {
        for cy in 0..cd[1] {
            for cx in 0..cd[0] {
                let mut on = false;
                'block: for dz in 0..stride[2] {
                    let z = cz * stride[2] + dz;
                    if z >= dims[2] {
                        break;
                    }
                    for dy in 0..stride[1] {
                        let y = cy * stride[1] + dy;
                        if y >= dims[1] {
                            break;
                        }
                        for dx in 0..stride[0] {
                            let x = cx * stride[0] + dx;
                            if x >= dims[0] {
                                break;
                            }
                            if volume.value(x, y, z) > threshold {
                                on = true;
                                break 'block;
                            }
                        }
                    }
                }
                fg[(cz * cd[1] + cy) * cd[0] + cx] = on as u8;
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
    for i in 0..cn {
        if head[i] == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        if volume.value(x, y, z) >= params.bone_hu {
            bone[i] = 1;
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

/// The protected intracranial core: `cavity ∪ vault`, reduced to the component
/// containing the brain, optionally dilated by `protect_radius` coarse cells so
/// the cut keeps a safety band clear of the brain and vault. Nothing in this grid
/// is ever removed by any region.
fn protected_core(
    cavity: &[u8],
    vault: &[u8],
    brain: &[u8],
    cd: [usize; 3],
    protect_radius: u32,
) -> Vec<u8> {
    let cn = cd[0] * cd[1] * cd[2];
    let mut core = vec![0u8; cn];
    for i in 0..cn {
        if cavity[i] != 0 || vault[i] != 0 {
            core[i] = 1;
        }
    }
    if core.iter().all(|&b| b == 0) {
        // No cavity/vault: protect the brain itself.
        core.copy_from_slice(brain);
    }
    // Seal the cavity-to-vault gap, keep the brain's component, then add the
    // safety band.
    core = dilate(&core, cd, 2);
    core = largest_component(&core, cd);
    if protect_radius > 0 {
        core = dilate(&core, cd, protect_radius);
    }
    core
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
    let n = nx * ny * nz;
    // Normalised distance from the boundary (0 = boundary, >=1 = fully removed).
    let mut dist = vec![f32::INFINITY; n];
    let mut queue: VecDeque<usize> = VecDeque::new();

    // Seed: removed voxels adjacent to a kept voxel (or the volume border), or
    // adjacent to a coarse keep cell (the true boundary is the coarse `core`).
    for z in 0..nz {
        let cz = (z / stride[2]).min(cd[2] - 1);
        for y in 0..ny {
            let cy = (y / stride[1]).min(cd[1] - 1);
            for x in 0..nx {
                let i = idx(x, y, z);
                if mask.remove[i] == 0 {
                    continue;
                }
                let cx = (x / stride[0]).min(cd[0] - 1);
                // A removed voxel sitting on a coarse keep cell is boundary.
                let mut boundary =
                    !core.is_empty() && core[(cz * cd[1] + cy) * cd[0] + cx] != 0;
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
                    dist[i] = 0.0;
                    queue.push_back(i);
                }
            }
        }
    }

    // 6-connected BFS with anisotropic step cost; keep the min distance.
    while let Some(i) = queue.pop_front() {
        let x = i % nx;
        let y = (i / nx) % ny;
        let z = i / (nx * ny);
        let d = dist[i];
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
            if nd < dist[j] {
                dist[j] = nd;
                queue.push_back(j);
            }
        }
    }

    // weight = clamp(distance, 0..1); 0 at the boundary, 1 at/beyond smooth_mm.
    mask.ensure_weight();
    for i in 0..n {
        if mask.remove[i] == 0 {
            continue;
        }
        let d = if dist[i].is_finite() { dist[i] } else { 1.0 };
        mask.weight[i] = d.clamp(0.0, 1.0);
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
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for i in 0..cn {
        if head[i] == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let v = volume.value(x, y, z);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let band = params.mr_brain_band;
    let mr_lo = lo + (hi - lo) * band.0 as f32;
    let mr_hi = lo + (hi - lo) * band.1 as f32;

    // Seed: coarse head voxel nearest the head centroid that is *not* skull
    // (on CT), so growth starts inside the cranial cavity.
    let head_centroid = coarse_centroid(volume, head, cd, stride);
    let mut seed = 0usize;
    let mut best_d = f64::MAX;
    for i in 0..cn {
        if head[i] == 0 {
            continue;
        }
        if is_ct && skull[i] != 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let p = volume.world(x, y, z);
        let d = dot(sub(p, head_centroid), sub(p, head_centroid));
        if d < best_d {
            best_d = d;
            seed = i;
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
    let accept = |i: usize| -> bool {
        if is_ct && skull[i] != 0 {
            return false; // skull is a hard barrier
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let v = volume.value(x, y, z);
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
        let mut visit = |nx: isize, ny: isize, nz: isize, grid: &mut Vec<u8>| {
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
            if head[ni] != 0 && grid[ni] == 0 && accept(ni) {
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
    for i in 0..cn {
        if brain[i] == 0 {
            continue;
        }
        let (x, y, z) = coarse_to_voxel(i, cd, stride, volume.dims);
        let p = volume.world(x, y, z);
        let d = sub(p, brain_center);
        let e = (d[0] / brain_semi[0]).powi(2)
            + (d[1] / brain_semi[1]).powi(2)
            + (d[2] / brain_semi[2]).powi(2);
        if e > 1.0 {
            brain[i] = 0;
        }
    }
    (brain, skull, vault, cavity)
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

/// 6-connected binary dilation with a cube of the given radius.
fn dilate(grid: &[u8], cd: [usize; 3], radius: u32) -> Vec<u8> {
    if radius == 0 {
        return grid.to_vec();
    }
    let r = radius as isize;
    let mut out = grid.to_vec();
    let cn = cd[0] * cd[1] * cd[2];
    for i in 0..cn {
        if grid[i] == 0 {
            continue;
        }
        let cz = (i / (cd[0] * cd[1])) as isize;
        let rem = i % (cd[0] * cd[1]);
        let cy = (rem / cd[0]) as isize;
        let cx = (rem % cd[0]) as isize;
        for dz in -r..=r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() + dy.abs() + dz.abs() > r {
                        continue;
                    }
                    let (nx, ny, nz) = (cx + dx, cy + dy, cz + dz);
                    if nx < 0 || ny < 0 || nz < 0 {
                        continue;
                    }
                    let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                    if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                        continue;
                    }
                    out[(nz * cd[1] + ny) * cd[0] + nx] = 1;
                }
            }
        }
    }
    out
}

/// 6-connected binary erosion with a cube of the given radius.
fn erode(grid: &[u8], cd: [usize; 3], radius: u32) -> Vec<u8> {
    if radius == 0 {
        return grid.to_vec();
    }
    let r = radius as isize;
    let mut out = grid.to_vec();
    let cn = cd[0] * cd[1] * cd[2];
    for i in 0..cn {
        if grid[i] == 0 {
            continue;
        }
        let cz = (i / (cd[0] * cd[1])) as isize;
        let rem = i % (cd[0] * cd[1]);
        let cy = (rem / cd[0]) as isize;
        let cx = (rem % cd[0]) as isize;
        let mut all_set = true;
        'outer: for dz in -r..=r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() + dy.abs() + dz.abs() > r {
                        continue;
                    }
                    let (nx, ny, nz) = (cx + dx, cy + dy, cz + dz);
                    if nx < 0 || ny < 0 || nz < 0 {
                        // Treat outside as foreground so a closing never erodes
                        // cells that merely touch the volume border.
                        continue;
                    }
                    let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                    if nx >= cd[0] || ny >= cd[1] || nz >= cd[2] {
                        continue;
                    }
                    if grid[(nz * cd[1] + ny) * cd[0] + nx] == 0 {
                        all_set = false;
                        break 'outer;
                    }
                }
            }
        }
        if !all_set {
            out[i] = 0;
        }
    }
    out
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
/// dilated by `protect_mm` so the cut keeps a safety band clear of the brain.
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
    let cell_mm = (0..3)
        .map(|a| seg.stride[a] as f64 * volume.spacing[a].max(1e-3))
        .fold(0.0f64, f64::max)
        .max(1e-3);
    let radius = (protect_mm.max(0.0) / cell_mm).ceil() as u32;
    let core = protected_core(&seg.cavity, &seg.vault, &seg.brain, seg.dims, radius);

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
    /// **either** region, so the cut can never extend into the brain or vault.
    /// Larger values are safer (and remove slightly less face); `0` disables the
    /// extra band. Default `2.0`.
    pub brain_protect_mm: f64,
    /// Feather width (mm) for [`MaskRegion::ExternalSoftTissue`] ("deflesh").
    /// Instead of a hard binary wall at the keep boundary, removed voxels within
    /// this distance of the boundary are blended toward the fill by a weight that
    /// ramps from `0` at the boundary to `1` this far inside — so the defleshed
    /// surface fades smoothly rather than ending in a flat cut. `0` disables the
    /// feather (hard cut); the default `3.0` gives a soft edge.
    pub deflesh_smooth_mm: f64,
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
        let mut most_anterior_brain = f64::MAX;
        for z in 0..seg.dims[2] {
            for y in 0..seg.dims[1] {
                for x in 0..seg.dims[0] {
                    if !seg.brain_at(x, y, z) {
                        continue;
                    }
                    let (vx, vy, vz) =
                        coarse_to_voxel(seg.cidx(x, y, z), seg.dims, seg.stride, volume.dims);
                    let py = volume.world(vx, vy, vz)[1];
                    if py < most_anterior_brain {
                        most_anterior_brain = py;
                    }
                }
            }
        }
        if most_anterior_brain == f64::MAX {
            return Err("segmentation found no brain region; refusing to deface".to_string());
        }

        // Most-anterior vault point (patient Y) — the frontal bone.
        let mut most_anterior_vault = f64::MAX;
        for z in 0..seg.dims[2] {
            for y in 0..seg.dims[1] {
                for x in 0..seg.dims[0] {
                    if !seg.vault_at(x, y, z) {
                        continue;
                    }
                    let (vx, vy, vz) =
                        coarse_to_voxel(seg.cidx(x, y, z), seg.dims, seg.stride, volume.dims);
                    let py = volume.world(vx, vy, vz)[1];
                    if py < most_anterior_vault {
                        most_anterior_vault = py;
                    }
                }
            }
        }

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
        let cell_mm = (0..3)
            .map(|a| seg.stride[a] as f64 * volume.spacing[a].max(1e-3))
            .fold(0.0f64, f64::max)
            .max(1e-3);
        let protect_radius = (self.params.brain_protect_mm / cell_mm).ceil().max(0.0) as u32;
        let core = protected_core(&seg.cavity, &seg.vault, &seg.brain, seg.dims, protect_radius);

        let mut mask = Mask::new(volume.dims);
        let mut removed = 0usize;

        if self.params.region == MaskRegion::ExternalSoftTissue {
            // "Deflesh": remove external soft tissue and bone — everything
            // **outside the cranial vault** — while keeping the intracranial
            // contents (brain, CSF) and the vault that encloses them. The keep
            // boundary is the protected core (`cavity ∪ vault` + safety band), so
            // it stops at the outer table of the calvarium and never reaches into
            // the face (facial bones are not part of the vault) or the brain.
            let cn = seg.dims[0] * seg.dims[1] * seg.dims[2];
            let keep = &core;
            let _ = cn;

            // Only strip the anterior face/scalp by default: keep external tissue
            // posterior of `brain_centroid_y + deflesh_posterior_mm` (the posterior
            // scalp, neck and back of the head). With `INFINITY` this gate is
            // inactive and all external tissue is removed.
            let posterior_limit = seg.brain_centroid[1] + self.params.deflesh_posterior_mm;

            for z in 0..volume.nz() {
                let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                for y in 0..volume.ny() {
                    let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                    for x in 0..volume.nx() {
                        if volume.value(x, y, z) <= threshold {
                            continue;
                        }
                        let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                        let kept = keep[seg.cidx(cx, cy, cz)] != 0;
                        if kept {
                            continue;
                        }
                        if volume.world(x, y, z)[1] >= posterior_limit {
                            continue; // posterior external tissue is kept (neck)
                        }
                        mask.set(x, y, z, true);
                        removed += 1;
                    }
                }
            }

            // Optional feather: ramp the removal weight from 0 at the kept
            // boundary to 1 `deflesh_smooth_mm` inside, so the surface fades
            // instead of ending in a hard wall. Only the boundary voxels change;
            // the binary `remove` set (and the safety guarantee) is unaffected.
            if self.params.deflesh_smooth_mm > 0.0 {
                feather_boundary(
                    &mut mask,
                    volume,
                    self.params.deflesh_smooth_mm,
                    &core,
                    seg.dims,
                    seg.stride,
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

        for z in 0..volume.nz() {
            let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
            for y in 0..volume.ny() {
                let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                for x in 0..volume.nx() {
                    if volume.value(x, y, z) <= threshold {
                        continue;
                    }
                    let py = volume.world(x, y, z)[1];
                    if py >= cut_y {
                        continue;
                    }
                    // Hard guard: never remove anything in the protected core
                    // (brain/vault + safety band), even if the coarse plane says so.
                    let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                    if core[seg.cidx(cx, cy, cz)] != 0 {
                        continue;
                    }
                    mask.set(x, y, z, true);
                    removed += 1;
                }
            }
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
