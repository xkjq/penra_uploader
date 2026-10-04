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

    /// Number of voxels whose rescaled value exceeds `threshold`.
    pub fn count_above(&self, threshold: f32) -> usize {
        (0..self.nz())
            .flat_map(|z| (0..self.ny()).flat_map(move |y| (0..self.nx()).map(move |x| (x, y, z))))
            .filter(|&(x, y, z)| self.value(x, y, z) > threshold)
            .count()
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
