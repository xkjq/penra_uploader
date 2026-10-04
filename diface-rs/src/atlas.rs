//! Atlas/template-based defacing backend.
//!
//! Registers a simple brain template to the subject volume using a 9-DOF affine
//! (per-axis scale + rotation + translation) fitted from the head's principal
//! axes and extents, then blanks foreground voxels that fall outside the
//! registered brain mask (limited to the anterior side so the neck is spared).
//!
//! The atlas is loaded from a file so nothing large is bundled. The file format
//! is a compact, self-describing little-endian layout (see [`Atlas::from_bytes`]):
//!
//! ```text
//! magic:   b"DFATLAS1"            (8 bytes)
//! version: u32 = 1
//! dims:    [u32; 3]               (nx, ny, nz) in voxels, x=left, y=posterior, z=superior
//! spacing: [f32; 3]               (mm per voxel along x, y, z)
//! origin:  [f32; 3]               (patient coordinate of voxel 0,0,0)
//! then nx*ny*nz bytes: 0 = not brain, 255 = brain
//! ```

use crate::backend::DefacingBackend;
use crate::geometry::{add, normalize, scale, sub, AXIS_ANTERIOR, AXIS_LEFT, AXIS_SUPERIOR, V3};
use crate::volume::{Mask, Volume};
use std::path::PathBuf;

/// A brain mask template.
#[derive(Clone, Debug)]
pub struct Atlas {
    pub dims: [usize; 3],
    pub spacing: [f64; 3],
    pub origin: V3,
    /// 1 = brain, 0 = outside. Row-major `(z*ny + y)*nx + x`.
    pub brain: Vec<u8>,
}

impl Atlas {
    #[inline]
    pub fn nx(&self) -> usize {
        self.dims[0]
    }
    #[inline]
    pub fn ny(&self) -> usize {
        self.dims[1]
    }
    #[inline]
    pub fn nz(&self) -> usize {
        self.dims[2]
    }

    #[inline]
    pub fn is_brain(&self, x: usize, y: usize, z: usize) -> bool {
        x < self.nx()
            && y < self.ny()
            && z < self.nz()
            && self.brain[(z * self.ny() + y) * self.nx() + x] != 0
    }

    /// Parse an atlas from [`Atlas::to_bytes`] format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Atlas, String> {
        if bytes.len() < 8 + 4 + 12 + 12 + 12 {
            return Err("atlas file too short".to_string());
        }
        if &bytes[0..8] != b"DFATLAS1" {
            return Err("bad atlas magic".to_string());
        }
        let rd_u32 =
            |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let rd_f32 =
            |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let version = rd_u32(8);
        if version != 1 {
            return Err(format!("unsupported atlas version {version}"));
        }
        let dims = [
            rd_u32(12) as usize,
            rd_u32(16) as usize,
            rd_u32(20) as usize,
        ];
        let spacing = [rd_f32(24) as f64, rd_f32(28) as f64, rd_f32(32) as f64];
        let origin = [rd_f32(36) as f64, rd_f32(40) as f64, rd_f32(44) as f64];
        let n = dims[0] * dims[1] * dims[2];
        let body = &bytes[48..];
        if body.len() < n {
            return Err(format!(
                "atlas body too short: {} bytes for {} voxels",
                body.len(),
                n
            ));
        }
        Ok(Atlas {
            dims,
            spacing,
            origin,
            brain: body[..n].to_vec(),
        })
    }

    /// Serialise to the [`Atlas::from_bytes`] format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48 + self.brain.len());
        out.extend_from_slice(b"DFATLAS1");
        out.extend_from_slice(&1u32.to_le_bytes());
        for d in self.dims {
            out.extend_from_slice(&(d as u32).to_le_bytes());
        }
        for s in self.spacing {
            out.extend_from_slice(&(s as f32).to_le_bytes());
        }
        for o in self.origin {
            out.extend_from_slice(&(o as f32).to_le_bytes());
        }
        out.extend_from_slice(&self.brain);
        out
    }

    /// Build a synthetic ellipsoidal "brain" atlas in a standard orientation:
    /// x=left, y=posterior, z=superior, centred at the origin. Useful as a
    /// built-in fallback and for tests.
    pub fn synthetic(dims: [usize; 3], spacing_mm: f64) -> Atlas {
        let [nx, ny, nz] = dims;
        let mut brain = vec![0u8; nx * ny * nz];
        let center = [
            (nx as f64 - 1.0) / 2.0,
            (ny as f64 - 1.0) / 2.0,
            (nz as f64 - 1.0) / 2.0,
        ];
        // Brain half-extents in mm (roughly a real brain).
        let half_mm = [65.0, 80.0, 70.0];
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let d = [
                        (x as f64 - center[0]) * spacing_mm / half_mm[0],
                        (y as f64 - center[1]) * spacing_mm / half_mm[1],
                        (z as f64 - center[2]) * spacing_mm / half_mm[2],
                    ];
                    if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= 1.0 {
                        brain[(z * ny + y) * nx + x] = 1;
                    }
                }
            }
        }
        Atlas {
            dims,
            spacing: [spacing_mm, spacing_mm, spacing_mm],
            origin: [
                -center[0] * spacing_mm,
                -center[1] * spacing_mm,
                -center[2] * spacing_mm,
            ],
            brain,
        }
    }
}

/// A fitted affine mapping atlas voxel coordinates to subject patient space.
#[derive(Clone, Copy, Debug)]
struct Affine {
    /// Patient coordinate of atlas voxel (0,0,0).
    origin: V3,
    /// Patient-space step per atlas voxel along x, y, z.
    step: [V3; 3],
}

impl Affine {
    #[allow(dead_code)]
    fn apply(&self, x: f64, y: f64, z: f64) -> V3 {
        add(
            self.origin,
            add(
                scale(self.step[0], x),
                add(scale(self.step[1], y), scale(self.step[2], z)),
            ),
        )
    }
}

/// Parameters for [`AtlasBackend`].
#[derive(Clone, Debug)]
pub struct AtlasParams {
    /// Foreground threshold (same semantics as the geometric backend).
    pub threshold: crate::geometric::Threshold,
    /// Remove-atlas-anterior-only: restrict removal to voxels anterior of the
    /// registered brain centre, so the neck is never touched.
    pub anterior_only: bool,
    /// Extra anterior margin (mm): brain mask is dilated forward by this much
    /// before deciding what is "brain", giving a safety buffer.
    pub brain_margin_mm: f64,
    /// Reject a mask that removes more than this fraction of all voxels.
    pub max_removed_fraction: f64,
    /// Floor for the estimated half-extents, in millimetres.
    pub min_extent_mm: f64,
}

impl Default for AtlasParams {
    fn default() -> Self {
        AtlasParams {
            threshold: crate::geometric::Threshold::Auto,
            anterior_only: true,
            brain_margin_mm: 8.0,
            max_removed_fraction: 0.6,
            min_extent_mm: 10.0,
        }
    }
}

/// How to obtain the atlas for [`AtlasBackend`].
#[derive(Clone, Debug)]
pub enum AtlasSpec {
    /// A built-in ellipsoidal brain template, in standard orientation. No files
    /// required; useful as a fallback and for quick comparison.
    Synthetic { dims: [usize; 3], spacing_mm: f64 },
    /// Load a brain mask from a `.dfatlas` file (see [`Atlas::from_bytes`]).
    File(PathBuf),
    /// Use an already-constructed atlas.
    InMemory(Atlas),
}

impl AtlasSpec {
    /// Load/build the atlas and construct the backend.
    pub fn build(&self) -> AtlasBackend {
        let atlas = self
            .load()
            .unwrap_or_else(|_| Atlas::synthetic([128, 160, 140], 1.0));
        AtlasBackend::new(atlas, AtlasParams::default())
    }

    pub fn load(&self) -> Result<Atlas, String> {
        match self {
            AtlasSpec::Synthetic { dims, spacing_mm } => Ok(Atlas::synthetic(*dims, *spacing_mm)),
            AtlasSpec::InMemory(a) => Ok(a.clone()),
            AtlasSpec::File(path) => {
                let bytes = std::fs::read(path)
                    .map_err(|e| format!("failed to read atlas {}: {}", path.display(), e))?;
                Atlas::from_bytes(&bytes)
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            AtlasSpec::Synthetic { .. } => "atlas:synthetic",
            AtlasSpec::File(_) => "atlas:file",
            AtlasSpec::InMemory(_) => "atlas:memory",
        }
    }
}

/// Atlas/template defacing backend.
pub struct AtlasBackend {
    atlas: Atlas,
    pub params: AtlasParams,
}

impl AtlasBackend {
    pub fn new(atlas: Atlas, params: AtlasParams) -> Self {
        AtlasBackend { atlas, params }
    }

    /// Fit a 9-DOF affine from the subject head to the atlas brain cloud.
    ///
    /// Uses the head's estimated centroid/axes/extents (subject) and the atlas
    /// brain's own bounding box (standard orientation), matching centres and
    /// axis half-extents. This is a coarse similarity transform, which is
    /// sufficient because the atlas mask is a generously-sized brain ellipsoid.
    fn fit(&self, volume: &Volume) -> Result<Affine, String> {
        let threshold = crate::geometric::choose_threshold(volume, &self.params.threshold);
        let stats = crate::geometric::head_statistics_best(volume, threshold, 160)
            .ok_or_else(|| "no head-like foreground found for atlas registration".to_string())?;

        // Atlas brain bounding box in patient space (its `origin` is voxel 0,0,0).
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for z in 0..self.atlas.nz() {
            for y in 0..self.atlas.ny() {
                for x in 0..self.atlas.nx() {
                    if self.atlas.is_brain(x, y, z) {
                        let p = [
                            self.atlas.origin[0] + x as f64 * self.atlas.spacing[0],
                            self.atlas.origin[1] + y as f64 * self.atlas.spacing[1],
                            self.atlas.origin[2] + z as f64 * self.atlas.spacing[2],
                        ];
                        for a in 0..3 {
                            lo[a] = lo[a].min(p[a]);
                            hi[a] = hi[a].max(p[a]);
                        }
                    }
                }
            }
        }
        if lo[0] > hi[0] {
            return Err("atlas brain mask is empty".to_string());
        }
        let atlas_center = [
            (lo[0] + hi[0]) / 2.0,
            (lo[1] + hi[1]) / 2.0,
            (lo[2] + hi[2]) / 2.0,
        ];
        let atlas_half = [
            ((hi[0] - lo[0]) / 2.0).max(1.0),
            ((hi[1] - lo[1]) / 2.0).max(1.0),
            ((hi[2] - lo[2]) / 2.0).max(1.0),
        ];

        // Subject brain half-extents: use the head half-extents scaled by a
        // constant (the brain is smaller than the head).
        let brain_frac = 0.82f64;
        let subj_half = [
            (stats.half[0] * brain_frac).max(self.params.min_extent_mm),
            (stats.half[1] * brain_frac).max(self.params.min_extent_mm),
            (stats.half[2] * brain_frac).max(self.params.min_extent_mm),
        ];
        // Subject-space direction for each atlas axis. Atlas axes are
        // x = left, y = posterior (= -anterior), z = superior.
        let subj_dir = [
            normalize(stats.left).unwrap_or(AXIS_LEFT),
            normalize(scale(stats.anterior, -1.0)).unwrap_or(scale(AXIS_ANTERIOR, -1.0)),
            normalize(stats.superior).unwrap_or(AXIS_SUPERIOR),
        ];
        let scales = [
            subj_half[0] / atlas_half[0],
            subj_half[1] / atlas_half[1],
            subj_half[2] / atlas_half[2],
        ];

        // Per-atlas-voxel step vectors (direction * spacing * scale).
        let step = [
            scale(subj_dir[0], self.atlas.spacing[0] * scales[0]),
            scale(subj_dir[1], self.atlas.spacing[1] * scales[1]),
            scale(subj_dir[2], self.atlas.spacing[2] * scales[2]),
        ];

        // Place the atlas centre at the subject head centroid. `atlas_center` is
        // in mm, and `step[i] / spacing_i` is the subject-space direction*scale
        // per atlas mm, so `apply(center_mm)` lands on the centroid.
        let origin = sub(
            stats.centroid,
            add(
                scale(step[0], atlas_center[0] / self.atlas.spacing[0].max(0.001)),
                add(
                    scale(step[1], atlas_center[1] / self.atlas.spacing[1].max(0.001)),
                    scale(step[2], atlas_center[2] / self.atlas.spacing[2].max(0.001)),
                ),
            ),
        );

        Ok(Affine { origin, step })
    }

    /// Map a subject patient point back to atlas voxel coordinates.
    fn atlas_coords(&self, affine: &Affine, p: V3) -> V3 {
        // Solve the small 3x3 linear system step * [x,y,z] = p - origin.
        let m = [
            [affine.step[0][0], affine.step[1][0], affine.step[2][0]],
            [affine.step[0][1], affine.step[1][1], affine.step[2][1]],
            [affine.step[0][2], affine.step[1][2], affine.step[2][2]],
        ];
        let b = sub(p, affine.origin);
        invert3(m)
            .map(|inv| {
                [
                    inv[0][0] * b[0] + inv[0][1] * b[1] + inv[0][2] * b[2],
                    inv[1][0] * b[0] + inv[1][1] * b[1] + inv[1][2] * b[2],
                    inv[2][0] * b[0] + inv[2][1] * b[1] + inv[2][2] * b[2],
                ]
            })
            .unwrap_or([f64::NAN; 3])
    }
}

impl DefacingBackend for AtlasBackend {
    fn name(&self) -> &str {
        "atlas"
    }

    fn description(&self) -> String {
        format!(
            "Register {}x{}x{} brain template and blank non-brain foreground",
            self.atlas.nx(),
            self.atlas.ny(),
            self.atlas.nz()
        )
    }

    fn compute_mask(&self, volume: &Volume) -> Result<Mask, String> {
        if volume.is_empty() {
            return Err("cannot deface an empty volume".to_string());
        }
        let affine = self.fit(volume)?;
        let threshold = crate::geometric::choose_threshold(volume, &self.params.threshold);

        // Anterior surface of the atlas brain in atlas voxel coordinates (lower
        // y = more anterior). This is the boundary we cut in front of.
        let mut brain_min_y = self.atlas.ny();
        for z in 0..self.atlas.nz() {
            for y in 0..self.atlas.ny() {
                for x in 0..self.atlas.nx() {
                    if self.atlas.is_brain(x, y, z) && y < brain_min_y {
                        brain_min_y = y;
                    }
                }
            }
        }
        if brain_min_y == self.atlas.ny() {
            return Err("atlas brain mask is empty".to_string());
        }
        // Atlas brain centre along y (posterior axis).
        let mut brain_max_y = 0usize;
        for z in 0..self.atlas.nz() {
            for y in 0..self.atlas.ny() {
                for x in 0..self.atlas.nx() {
                    if self.atlas.is_brain(x, y, z) && y > brain_max_y {
                        brain_max_y = y;
                    }
                }
            }
        }
        let brain_center_y = (brain_min_y + brain_max_y) as f64 / 2.0;

        // Keep an extra anterior margin so we never graze the frontal cortex.
        let margin_vox = self.params.brain_margin_mm / self.atlas.spacing[1].max(0.001);
        // Never cut behind the brain centre, even if registration drifts: this
        // guarantees no posterior tissue (neck / occipital) is ever removed.
        let cut_y = (brain_min_y as f64 - margin_vox).min(brain_center_y);

        // Remove only foreground voxels that map to *in front of* the registered
        // brain (more anterior than `cut_y`). Everything at or behind the cut is
        // kept, so the brain (and the whole posterior head/neck) is untouched.
        let mut mask = Mask::new(volume.dims);
        let mut removed = 0usize;
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
                    let c = self.atlas_coords(&affine, world);
                    if c.iter().any(|v| v.is_nan()) {
                        continue;
                    }
                    if c[1] < cut_y {
                        mask.set(x, y, z, true);
                        removed += 1;
                    }
                }
            }
        }

        let max_removed = (self.params.max_removed_fraction * volume.len() as f64) as usize;
        if removed > max_removed {
            return Err(format!(
                "atlas backend would remove {} of {} voxels (> {:.0}%); refusing",
                removed,
                volume.len(),
                self.params.max_removed_fraction * 100.0
            ));
        }
        Ok(mask)
    }
}

/// Invert a 3x3 matrix (row-major), or `None` if singular.
fn invert3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}
