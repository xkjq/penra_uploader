//! Dependency-free geometric defacing backend.
//!
//! The geometry of the head (its principal superior-inferior axis, centre and
//! extents) is estimated from a coarse, threshold-based, largest-connected-
//! component analysis of the volume. A symmetric "preserve ellipsoid" is then
//! placed over that region (biased slightly posterior and superior, where the
//! brain sits) and every foreground voxel that lies anterior to the ellipsoid
//! is blanked.
//!
//! This removes the protruding facial surface (nose, mouth, chin, orbits,
//! cheeks) while the preserve ellipsoid protects intracranial tissue. It is
//! intentionally conservative: it is better to leave a little facial skin than
//! to clip brain. For maximal robustness use an atlas/model backend (see
//! [`crate::DefacingBackend`]).

use std::collections::VecDeque;

use crate::backend::DefacingBackend;
use crate::geometry::{
    add, cross, dot, normalize, percentile, reject, scale, sub, AXIS_ANTERIOR, AXIS_LEFT,
    AXIS_SUPERIOR, V3,
};
use crate::volume::{Mask, Volume};

/// How the foreground threshold is chosen.
#[derive(Clone, Debug)]
pub enum Threshold {
    /// Use a CT-specific air threshold for CT and Otsu's method otherwise.
    Auto,
    /// Otsu's method on the intensity histogram.
    Otsu,
    /// A fixed, user supplied threshold in (rescaled) intensity units.
    Manual(f32),
}

impl Default for Threshold {
    fn default() -> Self {
        Threshold::Auto
    }
}

/// The masking algorithm used to decide which voxels to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaceAlgorithm {
    /// Remove foreground voxels outside a "preserve ellipsoid" over the head.
    /// Tunable, roughly conforms to the head.
    Ellipsoid,
    /// Remove foreground voxels anterior to a single (optionally tilted) plane.
    /// Simple and predictable; the brain is protected purely by the plane's
    /// position.
    Plane,
    /// Remove foreground voxels anterior to a curved front that follows the
    /// brain surface (anterior boundary varies with superior/inferior and
    /// left/right position). Protects the temporal/frontal poles better than a
    /// flat plane.
    CurvedFront,
}

impl Default for DefaceAlgorithm {
    fn default() -> Self {
        DefaceAlgorithm::Ellipsoid
    }
}

impl DefaceAlgorithm {
    pub fn all() -> [DefaceAlgorithm; 3] {
        [
            DefaceAlgorithm::Ellipsoid,
            DefaceAlgorithm::Plane,
            DefaceAlgorithm::CurvedFront,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            DefaceAlgorithm::Ellipsoid => "Ellipsoid",
            DefaceAlgorithm::Plane => "Plane cut",
            DefaceAlgorithm::CurvedFront => "Curved front",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            DefaceAlgorithm::Ellipsoid => "Preserve ellipsoid over the head; removes outside it",
            DefaceAlgorithm::Plane => "Remove everything anterior to a single (tiltable) plane",
            DefaceAlgorithm::CurvedFront => {
                "Remove anterior to a curved front that follows the brain surface"
            }
        }
    }
}

/// Parameters controlling [`GeometricBackend`].
#[derive(Clone, Debug)]
pub struct GeometricParams {
    /// Masking algorithm.
    pub algorithm: DefaceAlgorithm,
    /// Foreground threshold selection.
    pub threshold: Threshold,
    /// Fraction of each head half-extent retained inside the preserve ellipsoid.
    /// Larger = safer for brain, less complete defacing. Default `0.72`.
    pub preserve_fraction: f64,
    /// Move the ellipsoid centre posteriorly by this fraction of the
    /// anterior-posterior head half-extent (the brain sits behind the face).
    pub anterior_bias: f64,
    /// Move the ellipsoid centre superiorly by this fraction of the
    /// superior-inferior head half-extent.
    pub superior_bias: f64,
    /// Only remove voxels at least this far anterior of the ellipsoid centre.
    pub anterior_plane_offset_mm: f64,
    /// Approximate maximum coarse-grid edge used to estimate head geometry.
    pub coarse_target: usize,
    /// Reject a mask that removes more than this fraction of all voxels.
    pub max_removed_fraction: f64,
    /// Reject a volume whose foreground fraction is below this value.
    pub min_foreground_fraction: f64,
    /// Floor for the estimated half-extents, in millimetres.
    pub min_extent_mm: f64,
    /// Optional manual orientation override. When set, these unit axes replace
    /// the automatically estimated head frame (the caller is responsible for
    /// orthonormality). `None` uses the automatic estimate.
    pub axis_override: Option<HeadAxes>,
    /// Yaw applied to the frame about the superior axis, in degrees. Positive
    /// rotates the anterior/left axes toward patient left. Applies to both the
    /// automatic and manual frames.
    pub yaw_deg: f64,
    /// Anterior/posterior shift of the preserve ellipsoid centre along the
    /// anterior axis, in millimetres (positive = move forward, revealing more
    /// of the face for removal).
    pub anterior_offset_mm: f64,
    /// Per-axis multipliers on the preserve ellipsoid half-extents
    /// `[anterior, left, superior]`. Larger = keep more (safer, removes less).
    pub extent_scale: [f64; 3],
}

/// A manual head frame (unit vectors) used to override the automatic estimate.
#[derive(Clone, Copy, Debug)]
pub struct HeadAxes {
    /// Unit anterior axis.
    pub anterior: V3,
    /// Unit left axis.
    pub left: V3,
    /// Unit superior axis.
    pub superior: V3,
}

impl Default for GeometricParams {
    fn default() -> Self {
        // Defaults match `DefacePreset::Balanced`. The default previously used a
        // smaller preserve ellipsoid (preserve_fraction 0.72) which trimmed more
        // brain than intended; `Balanced` keeps a larger margin.
        GeometricParams {
            algorithm: DefaceAlgorithm::default(),
            threshold: Threshold::Auto,
            preserve_fraction: 0.82,
            anterior_bias: 0.06,
            superior_bias: 0.05,
            anterior_plane_offset_mm: 0.0,
            coarse_target: 160,
            max_removed_fraction: 0.6,
            min_foreground_fraction: 0.001,
            min_extent_mm: 10.0,
            axis_override: None,
            yaw_deg: 0.0,
            anterior_offset_mm: 0.0,
            extent_scale: [1.02, 1.02, 1.00],
        }
    }
}

/// A named, quickly-selectable set of [`GeometricParams`].
///
/// Presets only touch the preserve-ellipsoid sizing/bias, so any manual
/// orientation (yaw/axis override) the caller set is preserved when a preset is
/// applied via [`GeometricParams::apply_preset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefacePreset {
    /// Keep the most brain: a large preserve ellipsoid. May leave some scalp
    /// near the edge of the cut. Recommended default.
    Conservative,
    /// A middle ground between brain safety and facial removal.
    Balanced,
    /// Remove the most facial surface; smallest brain margin.
    Thorough,
}

impl DefacePreset {
    /// All presets, safest-first.
    pub fn all() -> [DefacePreset; 3] {
        [
            DefacePreset::Conservative,
            DefacePreset::Balanced,
            DefacePreset::Thorough,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            DefacePreset::Conservative => "Conservative",
            DefacePreset::Balanced => "Balanced",
            DefacePreset::Thorough => "Thorough",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            DefacePreset::Conservative => {
                "Keep the most brain; may leave some scalp at the cut edge"
            }
            DefacePreset::Balanced => "Balance brain safety and facial removal",
            DefacePreset::Thorough => "Remove the most facial surface; smallest brain margin",
        }
    }

    /// Parameters for this preset (orientation settings left at defaults).
    pub fn params(self) -> GeometricParams {
        let mut p = GeometricParams::default();
        p.apply_preset(self);
        p
    }
}

impl GeometricParams {
    /// Which preset (if any) exactly matches the current sizing/bias fields.
    pub fn matching_preset(&self) -> Option<DefacePreset> {
        DefacePreset::all().into_iter().find(|preset| {
            let p = preset.params();
            self.preserve_fraction == p.preserve_fraction
                && self.anterior_bias == p.anterior_bias
                && self.superior_bias == p.superior_bias
                && self.anterior_offset_mm == p.anterior_offset_mm
                && self.extent_scale == p.extent_scale
        })
    }

    /// Apply a preset's sizing/bias, keeping any manual orientation
    /// (`yaw_deg`, `axis_override`) already set.
    pub fn apply_preset(&mut self, preset: DefacePreset) {
        let (preserve, a_bias, s_bias, extent) = match preset {
            // Safer: bigger ellipsoid (keeps more), pushed posteriorly so the
            // frontal poles sit well inside it.
            DefacePreset::Conservative => (0.95, 0.10, 0.05, [1.10, 1.10, 1.05]),
            DefacePreset::Balanced => (0.82, 0.06, 0.05, [1.02, 1.02, 1.00]),
            DefacePreset::Thorough => (0.68, 0.03, 0.05, [0.95, 0.95, 0.95]),
        };
        self.preserve_fraction = preserve;
        self.anterior_bias = a_bias;
        self.superior_bias = s_bias;
        self.extent_scale = extent;
        self.anterior_offset_mm = 0.0;
    }
}

/// The geometric backend.
pub struct GeometricBackend {
    pub params: GeometricParams,
}

impl GeometricBackend {
    pub fn new(params: GeometricParams) -> Self {
        GeometricBackend { params }
    }

    /// Build a backend for a named preset.
    pub fn from_preset(preset: DefacePreset) -> Self {
        GeometricBackend {
            params: preset.params(),
        }
    }
}

impl DefacingBackend for GeometricBackend {
    fn name(&self) -> &str {
        "geometric"
    }

    fn description(&self) -> String {
        "Threshold + principal-axis head geometry with a preserve ellipsoid".to_string()
    }

    fn compute_mask(&self, volume: &Volume) -> Result<Mask, String> {
        let p = &self.params;
        if volume.is_empty() {
            return Err("cannot deface an empty volume".to_string());
        }

        let threshold = choose_threshold(volume, &p.threshold);

        let stats = head_statistics_best(volume, threshold, p.coarse_target).ok_or_else(|| {
            "no head-like foreground found; check the threshold/modality".to_string()
        })?;

        let fg = volume.count_above(threshold);
        let min_fg = (p.min_foreground_fraction * volume.len() as f64).max(1.0) as usize;
        if fg < min_fg {
            return Err(format!(
                "foreground fraction too small ({} of {} voxels above {:.1}); refusing to deface",
                fg,
                volume.len(),
                threshold
            ));
        }

        // Resolve the head frame: manual override (plus yaw) or automatic.
        let (anterior, left, superior) = resolve_axes(&stats, p);

        // Shared geometry for the masking algorithms.
        let bias = p.anterior_bias * stats.half[0] + p.anterior_offset_mm;
        let center = add(
            stats.centroid,
            add(
                scale(anterior, -bias),
                scale(superior, p.superior_bias * stats.half[2]),
            ),
        );
        let semi = [
            (stats.half[0] * p.preserve_fraction * p.extent_scale[0]).max(p.min_extent_mm),
            (stats.half[1] * p.preserve_fraction * p.extent_scale[1]).max(p.min_extent_mm),
            (stats.half[2] * p.preserve_fraction * p.extent_scale[2]).max(p.min_extent_mm),
        ];
        // Anterior cut position relative to the biased centre along the anterior
        // axis (used by the plane and curved-front algorithms).
        let cut_a = semi[0] * 0.85;
        // Curved front: how far the cut recedes at the superior extremes (mm).
        let curve_recess = (semi[2] * 0.55).max(8.0);

        let remove_at = |world: V3| -> bool {
            let d = sub(world, center);
            let a = dot(d, anterior);
            match p.algorithm {
                DefaceAlgorithm::Ellipsoid => {
                    if a <= p.anterior_plane_offset_mm {
                        return false;
                    }
                    let l = dot(d, left);
                    let s = dot(d, superior);
                    let e = (a / semi[0]).powi(2) + (l / semi[1]).powi(2) + (s / semi[2]).powi(2);
                    e > 1.0
                }
                DefaceAlgorithm::Plane => {
                    // Remove everything anterior to a single plane, tilted by the
                    // manual yaw (baked into the axes) and offset by Depth.
                    a > cut_a + p.anterior_plane_offset_mm
                }
                DefaceAlgorithm::CurvedFront => {
                    // The cut sits furthest forward at the mid-height and recedes
                    // quadratically toward the top and bottom, following the
                    // front of the brain.
                    let s = dot(d, superior) / semi[2].max(1.0);
                    let l = dot(d, left) / semi[1].max(1.0);
                    let recess = curve_recess * (s * s + 0.35 * l * l).min(1.5);
                    let plane = cut_a - recess;
                    a > plane + p.anterior_plane_offset_mm
                }
            }
        };

        let mut mask = Mask::new(volume.dims);
        let mut removed: usize = 0;

        for z in 0..volume.nz() {
            let base_z = add(
                volume.origin,
                scale(volume.dir[2], z as f64 * volume.spacing[2]),
            );
            for y in 0..volume.ny() {
                let base_y = add(base_z, scale(volume.dir[1], y as f64 * volume.spacing[1]));
                for x in 0..volume.nx() {
                    if volume.value(x, y, z) <= threshold {
                        continue;
                    }
                    let world = add(base_y, scale(volume.dir[0], x as f64 * volume.spacing[0]));
                    if remove_at(world) {
                        mask.set(x, y, z, true);
                        removed += 1;
                    }
                }
            }
        }

        let max_removed = (p.max_removed_fraction * volume.len() as f64) as usize;
        if removed > max_removed {
            return Err(format!(
                "geometric backend would remove {} of {} voxels (> {:.0}%); refusing. \
                 Increase --preserve-fraction or lower it if the head detection failed.",
                removed,
                volume.len(),
                p.max_removed_fraction * 100.0
            ));
        }

        Ok(mask)
    }
}

/// Resolve the working head frame from the automatic estimate, an optional
/// manual override, and a yaw about the superior axis.
fn resolve_axes(stats: &HeadStats, p: &GeometricParams) -> (V3, V3, V3) {
    let (seed_anterior, seed_left, seed_superior) = match p.axis_override {
        Some(ax) => (ax.anterior, ax.left, ax.superior),
        None => (stats.anterior, stats.left, stats.superior),
    };
    // Normalise defensively so a slightly-off manual frame is still usable.
    let superior = normalize(seed_superior).unwrap_or(AXIS_SUPERIOR);
    let mut anterior = normalize(reject(seed_anterior, superior)).unwrap_or(AXIS_ANTERIOR);
    let mut left = normalize(cross(superior, anterior)).unwrap_or(seed_left);

    if p.yaw_deg.abs() > 1e-6 {
        let yaw = p.yaw_deg.to_radians();
        let (s, c) = yaw.sin_cos();
        // Rotate in the (anterior, left) plane about superior.
        let new_anterior = add(scale(anterior, c), scale(left, s));
        let new_left = add(scale(anterior, -s), scale(left, c));
        anterior = normalize(new_anterior).unwrap_or(anterior);
        left = normalize(new_left).unwrap_or(left);
    }

    (anterior, left, superior)
}

/// Statistics describing the estimated head geometry.
#[derive(Clone, Debug)]
pub struct HeadStats {
    /// Head centroid in patient coordinates.
    pub centroid: V3,
    /// Unit anterior axis.
    pub anterior: V3,
    /// Unit left axis.
    pub left: V3,
    /// Unit superior (principal) axis.
    pub superior: V3,
    /// Half-extents along `[anterior, left, superior]` in millimetres.
    pub half: [f64; 3],
}

/// Pick a foreground threshold.
pub fn choose_threshold(volume: &Volume, mode: &Threshold) -> f32 {
    match mode {
        Threshold::Manual(v) => *v,
        Threshold::Otsu => otsu(volume),
        Threshold::Auto => {
            if volume.modality.eq_ignore_ascii_case("CT") {
                // Air/bone window air threshold. Everything from fat to bone is
                // foreground; air and lung are not.
                -300.0
            } else {
                otsu(volume)
            }
        }
    }
}

/// Otsu's method over a 256-bin histogram of sampled rescaled values.
pub fn otsu(volume: &Volume) -> f32 {
    let n = volume.len();
    if n == 0 {
        return 0.0;
    }
    let step = (n / 1_000_000).max(1);
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let mut i = 0;
    while i < n {
        let z = i / (volume.nx() * volume.ny());
        let rem = i % (volume.nx() * volume.ny());
        let y = rem / volume.nx();
        let x = rem % volume.nx();
        let v = volume.value(x, y, z);
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        i += step;
    }
    if !(hi > lo) {
        return lo;
    }

    const BINS: usize = 256;
    let mut hist = [0f64; BINS];
    let mut total = 0f64;
    let mut i = 0;
    while i < n {
        let z = i / (volume.nx() * volume.ny());
        let rem = i % (volume.nx() * volume.ny());
        let y = rem / volume.nx();
        let x = rem % volume.nx();
        let v = volume.value(x, y, z);
        if v.is_finite() {
            let b = (((v - lo) / (hi - lo)) * (BINS - 1) as f32).round() as usize;
            hist[b.min(BINS - 1)] += 1.0;
            total += 1.0;
        }
        i += step;
    }

    let mut sum_all = 0f64;
    for (b, &h) in hist.iter().enumerate() {
        sum_all += b as f64 * h;
    }

    let mut w_b = 0f64;
    let mut sum_b = 0f64;
    let mut best_var = -1f64;
    let mut best_bin = 0usize;
    for (b, &h) in hist.iter().enumerate() {
        w_b += h;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += b as f64 * h;
        let m_b = sum_b / w_b;
        let m_f = (sum_all - sum_b) / w_f;
        let var = w_b * w_f * (m_b - m_f).powi(2);
        if var > best_var {
            best_var = var;
            best_bin = b;
        }
    }

    lo + (best_bin as f32 / (BINS - 1) as f32) * (hi - lo)
}

/// Estimate head geometry from the largest connected foreground component.
///
/// The volume is analysed on a coarse grid for speed and memory efficiency;
/// the resulting statistics are resolution independent.
pub fn head_statistics(volume: &Volume, threshold: f32, coarse_target: usize) -> Option<HeadStats> {
    head_statistics_impl(volume, threshold, coarse_target, None)
}

/// Head statistics using the in-house segmentation to isolate the head first
/// (best-effort), falling back to the plain estimate if segmentation fails.
///
/// This is the recommended entry point: on wide-FOV CT (head + neck/table) the
/// plain estimate's principal axis can be skewed, whereas the segmented head
/// confines the point cloud to the head.
pub fn head_statistics_best(
    volume: &Volume,
    threshold: f32,
    coarse_target: usize,
) -> Option<HeadStats> {
    let seg_params = crate::segmentation::SegParams {
        threshold: crate::geometric::Threshold::Manual(threshold),
        ..crate::segmentation::SegParams::default()
    };
    if let Ok(seg) = crate::segmentation::segment(volume, &seg_params) {
        if seg.head_count() > 0 {
            let filter = crate::segmentation::head_filter(&seg, volume);
            if let Some(stats) =
                head_statistics_impl(volume, threshold, coarse_target, Some(&filter))
            {
                return Some(stats);
            }
        }
    }
    head_statistics_impl(volume, threshold, coarse_target, None)
}

/// Like [`head_statistics`] but restricted to a caller-supplied head mask.
///
/// `head_filter(x, y, z)` returns whether the full-resolution voxel belongs to
/// the head. Restricting the point cloud to the segmented head removes the
/// neck/table leakage that skews the principal-axis estimate on wide-FOV scans.
pub fn head_statistics_masked(
    volume: &Volume,
    threshold: f32,
    coarse_target: usize,
    head_filter: &dyn Fn(usize, usize, usize) -> bool,
) -> Option<HeadStats> {
    head_statistics_impl(volume, threshold, coarse_target, Some(head_filter))
}

fn head_statistics_impl(
    volume: &Volume,
    threshold: f32,
    coarse_target: usize,
    head_filter: Option<&dyn Fn(usize, usize, usize) -> bool>,
) -> Option<HeadStats> {
    let target = coarse_target.max(8);
    let dims = volume.dims;
    let max_dim = dims.iter().copied().max().unwrap_or(1);
    let stride = ((max_dim + target - 1) / target).max(1);
    let stride = [stride; 3];
    let cd = [
        (dims[0] + stride[0] - 1) / stride[0],
        (dims[1] + stride[1] - 1) / stride[1],
        (dims[2] + stride[2] - 1) / stride[2],
    ];
    let cn = cd[0] * cd[1] * cd[2];
    if cn == 0 {
        return None;
    }

    // Coarse foreground occupancy: a block is foreground when any voxel in it
    // exceeds the threshold (and, when a head filter is supplied, belongs to the
    // head).
    let mut fg = vec![false; cn];
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
                            if volume.value(x, y, z) > threshold
                                && head_filter.map(|f| f(x, y, z)).unwrap_or(true)
                            {
                                on = true;
                                break 'block;
                            }
                        }
                    }
                }
                fg[(cz * cd[1] + cy) * cd[0] + cx] = on;
            }
        }
    }

    // Largest 6-connected component.
    let mut label = vec![0i32; cn];
    let mut best: Vec<usize> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut queue: VecDeque<usize> = VecDeque::new();
    for start in 0..cn {
        if !fg[start] || label[start] != 0 {
            continue;
        }
        current.clear();
        queue.clear();
        queue.push_back(start);
        label[start] = 1;
        while let Some(i) = queue.pop_front() {
            current.push(i);
            let cz = i / (cd[0] * cd[1]);
            let rem = i % (cd[0] * cd[1]);
            let cy = rem / cd[0];
            let cx = rem % cd[0];
            if cx > 0 {
                let ni = (cz * cd[1] + cy) * cd[0] + (cx - 1);
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
            if cx + 1 < cd[0] {
                let ni = (cz * cd[1] + cy) * cd[0] + (cx + 1);
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
            if cy > 0 {
                let ni = (cz * cd[1] + (cy - 1)) * cd[0] + cx;
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
            if cy + 1 < cd[1] {
                let ni = (cz * cd[1] + (cy + 1)) * cd[0] + cx;
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
            if cz > 0 {
                let ni = ((cz - 1) * cd[1] + cy) * cd[0] + cx;
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
            if cz + 1 < cd[2] {
                let ni = ((cz + 1) * cd[1] + cy) * cd[0] + cx;
                if fg[ni] && label[ni] == 0 {
                    label[ni] = 1;
                    queue.push_back(ni);
                }
            }
        }
        if current.len() > best.len() {
            std::mem::swap(&mut best, &mut current);
        }
    }

    if best.is_empty() {
        return None;
    }

    // World positions of the component's coarse voxels.
    let mut points: Vec<V3> = Vec::with_capacity(best.len());
    for &i in &best {
        let cz = i / (cd[0] * cd[1]);
        let rem = i % (cd[0] * cd[1]);
        let cy = rem / cd[0];
        let cx = rem % cd[0];
        let x = (cx * stride[0]).min(dims[0] - 1);
        let y = (cy * stride[1]).min(dims[1] - 1);
        let z = (cz * stride[2]).min(dims[2] - 1);
        points.push(volume.world(x, y, z));
    }

    let n = points.len() as f64;
    let mut centroid = [0.0; 3];
    for p in &points {
        centroid[0] += p[0];
        centroid[1] += p[1];
        centroid[2] += p[2];
    }
    centroid = scale(centroid, 1.0 / n);

    // Covariance and principal (superior-inferior) axis.
    let mut cov = [[0.0f64; 3]; 3];
    for p in &points {
        let d = sub(*p, centroid);
        for r in 0..3 {
            for c in 0..3 {
                cov[r][c] += d[r] * d[c];
            }
        }
    }
    for row in cov.iter_mut() {
        for v in row.iter_mut() {
            *v /= n;
        }
    }
    // Assign anatomical axes using explicit patient-axis priors rather than raw
    // variance. On wide-FOV or partially segmented scans (a head CT that also
    // imaged the neck/table, say) the in-plane variance can exceed the
    // superior-inferior variance, so "largest eigenvalue = superior" is
    // unreliable. Instead:
    //   left     = eigenvector most aligned with patient X (+left)
    //   anterior = eigenvector most aligned with patient Y (-posterior)
    //   superior = left x anterior  (should be ~ patient Z)
    // The dominant-variance eigenvector is still preferred when it already
    // aligns with the relevant patient axis.
    let eig = symmetric_eigen_3x3(cov);

    // Prior axes come from the volume's own frame: for an axial acquisition the
    // slice axis is the superior-inferior direction and the in-plane axes are
    // left-right / anterior-posterior. This is robust for tilted and oblique
    // stacks, unlike a global Z prior.
    let slice_axis = normalize(volume.dir[2]).unwrap_or(AXIS_SUPERIOR);
    let col_axis = normalize(volume.dir[0]).unwrap_or(AXIS_LEFT);
    // `dir[1]` is the direction of increasing row index = patient posterior.
    let row_axis = normalize(volume.dir[1]).unwrap_or(AXIS_ANTERIOR);

    let pick = |target: V3| -> V3 {
        let mut best: Option<(f64, V3)> = None;
        for (lambda, v) in eig {
            let align = dot(v, target).abs();
            let score = lambda.max(0.0) * align * align;
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, v));
            }
        }
        best.map(|(_, v)| v).unwrap_or(target)
    };

    // Anatomical assignment using the acquisition geometry:
    // - Axial stack (slice axis ≈ patient Z): superior = slice axis.
    // - Otherwise (coronal/sagittal stack): superior lies in-plane; find it via
    //   PCA among the axes most perpendicular to the slice normal.
    let sup = if dot(slice_axis, AXIS_SUPERIOR).abs() > 0.6 {
        // Axial-ish: superior is the slice advance direction.
        if dot(slice_axis, AXIS_SUPERIOR) < 0.0 {
            scale(slice_axis, -1.0)
        } else {
            slice_axis
        }
    } else {
        // In-plane superior: the eigenvector most aligned with world Z, which is
        // guaranteed to be (nearly) perpendicular to a non-axial slice normal.
        let v = pick(AXIS_SUPERIOR);
        if dot(v, AXIS_SUPERIOR) < 0.0 {
            scale(v, -1.0)
        } else {
            v
        }
    };
    let superior = normalize(sup).unwrap_or(AXIS_SUPERIOR);

    // Anterior: in-plane axis closest to the posterior row axis, negated and
    // orthogonalised against superior.
    let ant_seed = pick(row_axis);
    let mut anterior = normalize(reject(ant_seed, superior)).unwrap_or(AXIS_ANTERIOR);
    if dot(anterior, row_axis) > 0.0 {
        // Point toward the anterior (opposite the posterior row axis).
        anterior = scale(anterior, -1.0);
    }
    let mut left = normalize(cross(superior, anterior)).unwrap_or(col_axis);
    if dot(left, col_axis) < 0.0 {
        left = scale(left, -1.0);
    }
    let left = normalize(left).unwrap_or(col_axis);
    let axis = [anterior, left, superior];

    // Robust half-extents per anatomical axis.
    let mut half = [0.0f64; 3];
    for (ai, ax) in axis.iter().enumerate() {
        let mut proj: Vec<f64> = points.iter().map(|p| dot(sub(*p, centroid), *ax)).collect();
        proj.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p01 = percentile(&proj, 0.01);
        let p99 = percentile(&proj, 0.99);
        half[ai] = p99.max(-p01).max(1e-3);
    }

    Some(HeadStats {
        centroid,
        anterior,
        left,
        superior,
        half,
    })
}

/// Eigen-decomposition of a symmetric 3x3 matrix via the Jacobi method.
///
/// Returns `(eigenvalue, eigenvector)` pairs (vectors are unit length).
fn symmetric_eigen_3x3(a: [[f64; 3]; 3]) -> [(f64, V3); 3] {
    let mut m = a;
    // Eigenvector accumulator (columns are the current eigenvectors).
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    for _ in 0..24 {
        // Find the largest off-diagonal magnitude.
        let (mut p, mut q, mut max) = (0usize, 1usize, 0.0f64);
        for i in 0..3 {
            for j in (i + 1)..3 {
                if m[i][j].abs() > max {
                    max = m[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if max < 1e-12 {
            break;
        }
        let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;

        // Apply the rotation to m and accumulate it in v.
        let mut next = m;
        for k in 0..3 {
            next[p][k] = c * m[p][k] - s * m[q][k];
            next[q][k] = s * m[p][k] + c * m[q][k];
        }
        let mid = next;
        for k in 0..3 {
            next[k][p] = c * mid[k][p] - s * mid[k][q];
            next[k][q] = s * mid[k][p] + c * mid[k][q];
        }
        m = next;

        let mut nv = v;
        for k in 0..3 {
            nv[k][p] = c * v[k][p] - s * v[k][q];
            nv[k][q] = s * v[k][p] + c * v[k][q];
        }
        v = nv;
    }

    [
        (m[0][0], [v[0][0], v[1][0], v[2][0]]),
        (m[1][1], [v[0][1], v[1][1], v[2][1]]),
        (m[2][2], [v[0][2], v[1][2], v[2][2]]),
    ]
}
