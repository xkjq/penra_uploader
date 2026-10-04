//! Small 3D vector helpers used by `diface-rs`.
//!
//! All geometry is expressed in DICOM patient coordinates (LPS):
//! +X = patient left, +Y = patient posterior, +Z = patient superior.
//! Therefore the anatomical **anterior** direction is `-Y`.

pub type V3 = [f64; 3];

pub const AXIS_LEFT: V3 = [1.0, 0.0, 0.0];
pub const AXIS_POSTERIOR: V3 = [0.0, 1.0, 0.0];
pub const AXIS_SUPERIOR: V3 = [0.0, 0.0, 1.0];
/// Anterior is the opposite of posterior in patient coordinates.
pub const AXIS_ANTERIOR: V3 = [0.0, -1.0, 0.0];

#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// Normalise a vector, returning `None` when it is (near) zero length.
#[inline]
pub fn normalize(a: V3) -> Option<V3> {
    let n = norm(a);
    if n > 1e-9 {
        Some(scale(a, 1.0 / n))
    } else {
        None
    }
}

/// Return `v` projected onto the plane perpendicular to `axis`
/// (`axis` need not be unit length, but should be non-zero).
pub fn reject(v: V3, axis: V3) -> V3 {
    let n = normalize(axis).unwrap_or([0.0, 0.0, 1.0]);
    sub(v, scale(n, dot(v, n)))
}

/// A simple 1-based percentile over a sorted slice.
pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let q = q.clamp(0.0, 1.0);
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let t = pos - lo as f64;
        sorted[lo] * (1.0 - t) + sorted[hi] * t
    }
}
