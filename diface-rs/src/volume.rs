//! In-memory 3D volume and boolean removal mask.

use crate::geometry::{scale, V3};

/// A 3D DICOM series volume.
///
/// Voxel `(x, y, z)` is indexed as `x` = column, `y` = row, `z` = slice.
/// `data` holds the **raw stored sample values** (before rescale); use
/// [`Volume::value`] to obtain modality-rescaled intensities (Hounsfield
/// units for CT).
#[derive(Clone, Debug)]
pub struct Volume {
    /// Raw stored samples, laid out slice-major: `z*ny*nx + y*nx + x`.
    pub data: Vec<f32>,
    /// `[nx, ny, nz]`.
    pub dims: [usize; 3],
    /// Millimetres per voxel along `[x, y, z]`.
    pub spacing: [f64; 3],
    /// World (patient) coordinate of voxel `(0, 0, 0)`.
    pub origin: V3,
    /// Unit direction cosines for voxel axes `[x, y, z]` in patient space.
    pub dir: [V3; 3],
    /// Per-slice `(slope, intercept)` resale parameters.
    pub rescale: Vec<(f32, f32)>,
    /// Modality code, e.g. `CT` or `MR`.
    pub modality: String,
}

impl Volume {
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
    pub fn len(&self) -> usize {
        self.data.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.ny() + y) * self.nx() + x
    }

    /// Raw stored sample value (no rescale applied).
    #[inline]
    pub fn raw(&self, x: usize, y: usize, z: usize) -> f32 {
        self.data[self.idx(x, y, z)]
    }

    /// Modality-rescaled value (Hounsfield units for CT).
    #[inline]
    pub fn value(&self, x: usize, y: usize, z: usize) -> f32 {
        let raw = self.raw(x, y, z);
        let (slope, intercept) = self.rescale[z];
        raw * slope + intercept
    }

    /// World (patient) coordinate of voxel `(x, y, z)` in millimetres.
    #[inline]
    pub fn world(&self, x: usize, y: usize, z: usize) -> V3 {
        let mut p = self.origin;
        p = add3(p, scale(self.dir[0], x as f64 * self.spacing[0]));
        p = add3(p, scale(self.dir[1], y as f64 * self.spacing[1]));
        p = add3(p, scale(self.dir[2], z as f64 * self.spacing[2]));
        p
    }

    /// Coefficients of the linear form giving the patient coordinate along
    /// `axis`: `world(x, y, z)[axis] == base + step[0] * x + step[1] * y +
    /// step[2] * z`.
    ///
    /// Hot per-voxel loops use this to hoist the per-slice and per-row terms
    /// instead of building a full world vector for every voxel.
    #[inline]
    pub fn world_axis_linear(&self, axis: usize) -> (f64, [f64; 3]) {
        (
            self.origin[axis],
            [
                self.dir[0][axis] * self.spacing[0],
                self.dir[1][axis] * self.spacing[1],
                self.dir[2][axis] * self.spacing[2],
            ],
        )
    }

    /// Number of voxels whose rescaled value exceeds `threshold`.
    pub fn count_above(&self, threshold: f32) -> usize {
        let nxy = self.nx() * self.ny();
        let nz = self.nz();
        let mut count = 0usize;
        for z in 0..nz {
            let (slope, intercept) = self.rescale[z];
            let slice = &self.data[z * nxy..(z + 1) * nxy];
            count += slice
                .iter()
                .filter(|&&raw| raw * slope + intercept > threshold)
                .count();
        }
        count
    }

    /// Statistics over raw data: `(min, max)`.
    pub fn raw_min_max(&self) -> (f32, f32) {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for &v in &self.data {
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        (lo, hi)
    }
}

#[inline]
fn add3(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// A per-voxel removal mask. `remove[i] != 0` means "this voxel is removed /
/// blanked". `weight` optionally carries how strongly each removed voxel is
/// blended toward the fill value (`1.0` = fully removed, `< 1.0` = feathered),
/// so a boundary can fade instead of ending in a hard wall. When `weight` is
/// empty the removal is binary (`1.0` for every removed voxel).
#[derive(Clone, Debug)]
pub struct Mask {
    pub dims: [usize; 3],
    pub remove: Vec<u8>,
    /// Optional per-voxel removal strength in `0..=1`, same length as `remove`.
    pub weight: Vec<f32>,
}

impl Mask {
    pub fn new(dims: [usize; 3]) -> Self {
        let n = dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]);
        Mask {
            dims,
            remove: vec![0u8; n],
            weight: Vec::new(),
        }
    }

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
    pub fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.ny() + y) * self.nx() + x
    }

    #[inline]
    pub fn is_removed(&self, x: usize, y: usize, z: usize) -> bool {
        self.remove[self.idx(x, y, z)] != 0
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, remove: bool) {
        let i = self.idx(x, y, z);
        self.remove[i] = remove as u8;
    }

    /// Removal strength at a flat index: `1.0` unless a feathered `weight` says
    /// otherwise. Removed voxels with no explicit weight are fully removed.
    #[inline]
    pub fn weight_at(&self, i: usize) -> f32 {
        if self.weight.is_empty() {
            1.0
        } else {
            self.weight.get(i).copied().unwrap_or(1.0)
        }
    }

    /// Enable a feathered weight buffer (initialised to `1.0` everywhere).
    pub fn ensure_weight(&mut self) {
        if self.weight.len() != self.remove.len() {
            self.weight = vec![1.0f32; self.remove.len()];
        }
    }

    /// Set the removal strength for a voxel (enables the weight buffer lazily).
    #[inline]
    pub fn set_weight(&mut self, x: usize, y: usize, z: usize, w: f32) {
        self.ensure_weight();
        let i = self.idx(x, y, z);
        self.weight[i] = w.clamp(0.0, 1.0);
    }

    pub fn count_removed(&self) -> usize {
        self.remove.iter().filter(|&&v| v != 0).count()
    }

    pub fn count_removed_in_slice(&self, z: usize) -> usize {
        let start = z * self.nx() * self.ny();
        let end = start + self.nx() * self.ny();
        self.remove[start..end].iter().filter(|&&v| v != 0).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_volume() -> Volume {
        let dims = [7usize, 5, 4];
        let [nx, ny, nz] = dims;
        let angle = 0.3f64;
        let dir = [
            [1.0, 0.0, 0.0],
            [0.0, angle.cos(), angle.sin()],
            [0.0, -angle.sin(), angle.cos()],
        ];
        Volume {
            data: (0..nx * ny * nz).map(|i| (i as f32) * 7.0 - 90.0).collect(),
            dims,
            spacing: [0.7, 1.3, 2.5],
            origin: [11.0, -4.0, 3.0],
            dir,
            rescale: (0..nz)
                .map(|z| (1.0 + z as f32 * 0.1, z as f32 * 5.0))
                .collect(),
            modality: "CT".to_string(),
        }
    }

    #[test]
    fn count_above_matches_per_voxel_loop() {
        let volume = sample_volume();
        for threshold in [-200.0f32, -50.0, 0.0, 40.0, 400.0] {
            let mut naive = 0usize;
            for z in 0..volume.nz() {
                for y in 0..volume.ny() {
                    for x in 0..volume.nx() {
                        if volume.value(x, y, z) > threshold {
                            naive += 1;
                        }
                    }
                }
            }
            assert_eq!(
                volume.count_above(threshold),
                naive,
                "threshold {threshold}"
            );
        }
    }

    #[test]
    fn world_axis_linear_matches_world() {
        let volume = sample_volume();
        for axis in 0..3 {
            let (base, step) = volume.world_axis_linear(axis);
            for z in 0..volume.nz() {
                for y in 0..volume.ny() {
                    for x in 0..volume.nx() {
                        let expected = volume.world(x, y, z)[axis];
                        let got =
                            base + step[0] * x as f64 + step[1] * y as f64 + step[2] * z as f64;
                        assert!(
                            (got - expected).abs() < 1e-9,
                            "axis {axis} at ({x},{y},{z}): {got} vs {expected}"
                        );
                    }
                }
            }
        }
    }
}
