use eframe::egui;
use egui::Vec2;
use dicom_object::{open_file, Tag};
use dicom_pixeldata::PixelDecoder;
use diface_rs::{
    DefaceAlgorithm, DefacePreset, DefacingBackend, GeometricBackend, GeometricParams,
    Volume as DifaceVolume,
};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::fs::create_dir_all;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use directories::ProjectDirs;

const METADATA_TAGS: &[(Tag, &str)] = &[
    (Tag(0x0010, 0x0010), "Patient Name"),
    (Tag(0x0010, 0x0020), "Patient ID"),
    (Tag(0x0010, 0x0030), "Date of Birth"),
    (Tag(0x0010, 0x0040), "Sex"),
    (Tag(0x0008, 0x0060), "Modality"),
    (Tag(0x0008, 0x0020), "Study Date"),
    (Tag(0x0008, 0x103E), "Series Description"),
    (Tag(0x0008, 0x1030), "Study Description"),
    (Tag(0x0020, 0x0011), "Series Number"),
    (Tag(0x0020, 0x0013), "Instance Number"),
];

/// Standard CT window/level presets, in Hounsfield units (center, width).
const CT_WL_PRESETS: &[(&str, f32, f32)] = &[
    ("Brain", 40.0, 80.0),
    ("Bone", 300.0, 1500.0),
    ("Lung", -600.0, 1500.0),
    ("Abdomen", 40.0, 400.0),
    ("Mediastinum", 50.0, 350.0),
];

/// Wheel input (in egui points) required before advancing one slice. `smooth_scroll_delta`
/// delivers a decaying tail after each physical notch, so stepping directly on every
/// non-zero delta scrolls several slices per notch; accumulating to this threshold and
/// stepping at most one slice per frame keeps wheel scrolling predictable.
const WHEEL_SLICE_THRESHOLD: f32 = 48.0;

/// In-memory decoded pixels before rendering (allows efficient W/L re-renders).
#[derive(Clone)]
struct Gray16 {
    data: Vec<f32>, // rescaled (HU for CT, raw*slope+intercept for others)
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Stack,
    Mpr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MprPlane {
    Axial,
    Coronal,
    Sagittal,
}

impl MprPlane {
    fn label(self) -> &'static str {
        match self {
            MprPlane::Axial => "Axial",
            MprPlane::Coronal => "Coronal",
            MprPlane::Sagittal => "Sagittal",
        }
    }

    /// `(u, v, w)` axes for this plane, where `u` runs left→right in the image,
    /// `v` runs top→bottom, and `w` is the through-plane (slice) axis.
    ///
    /// Signs follow the standard radiological convention so the image is
    /// presented upright:
    /// - Axial: anterior at the top (v = +Posterior).
    /// - Coronal: superior at the top (v = -Superior), left on the left.
    /// - Sagittal: superior at the top (v = -Superior), anterior on the left
    ///   (u = +Posterior, so increasing Y is to the right).
    fn patient_axes(self) -> (PatientAxis, PatientAxis, PatientAxis) {
        match self {
            MprPlane::Axial => (PatientAxis::X, PatientAxis::Y, PatientAxis::Z),
            MprPlane::Coronal => (PatientAxis::X, PatientAxis::Z, PatientAxis::Y),
            MprPlane::Sagittal => (PatientAxis::Y, PatientAxis::Z, PatientAxis::X),
        }
    }

    /// Sign applied to the `u` axis of [`patient_axes`] when sampling.
    fn u_sign(self) -> f32 {
        // Sagittal `u` axis is +Y (posterior); leaving it unflipped puts
        // increasing Y (posterior) to the right and anterior to the left, which
        // is the standard radiological convention.
        1.0
    }

    /// Sign applied to the `v` axis of [`patient_axes`] when sampling.
    fn v_sign(self) -> f32 {
        match self {
            // Axial: posterior is at the bottom of the image.
            MprPlane::Axial => 1.0,
            // Coronal and sagittal: superior is at the top of the image.
            MprPlane::Coronal | MprPlane::Sagittal => -1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PatientAxis {
    X,
    Y,
    Z,
}

impl PatientAxis {
    fn index(self) -> usize {
        match self {
            PatientAxis::X => 0,
            PatientAxis::Y => 1,
            PatientAxis::Z => 2,
        }
    }

    fn unit(self) -> [f32; 3] {
        match self {
            PatientAxis::X => [1.0, 0.0, 0.0],
            PatientAxis::Y => [0.0, 1.0, 0.0],
            PatientAxis::Z => [0.0, 0.0, 1.0],
        }
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(v: [f32; 3], factor: f32) -> [f32; 3] {
    [v[0] * factor, v[1] * factor, v[2] * factor]
}

fn magnitude(v: [f32; 3]) -> f32 {
    dot(v, v).sqrt()
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let mag = magnitude(v);
    (mag > 1e-6).then_some(scale(v, 1.0 / mag))
}

fn rotate_vec2(v: egui::Vec2, angle_rad: f32) -> egui::Vec2 {
    let (sin_a, cos_a) = angle_rad.sin_cos();
    egui::vec2(v.x * cos_a - v.y * sin_a, v.x * sin_a + v.y * cos_a)
}

fn paint_rotated_texture(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    center: egui::Pos2,
    size: egui::Vec2,
    angle_rad: f32,
) {
    let half = size * 0.5;
    let corners = [
        egui::vec2(-half.x, -half.y),
        egui::vec2(half.x, -half.y),
        egui::vec2(half.x, half.y),
        egui::vec2(-half.x, half.y),
    ];
    let uvs = [
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ];

    let mut mesh = egui::epaint::Mesh::with_texture(texture_id);
    for (corner, uv) in corners.iter().zip(uvs.iter()) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + rotate_vec2(*corner, angle_rad),
            uv: *uv,
            color: egui::Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

fn axis_aligned_patient_point(
    u_axis: PatientAxis,
    v_axis: PatientAxis,
    w_axis: PatientAxis,
    u: f32,
    v: f32,
    w: f32,
) -> [f32; 3] {
    let mut point = [0.0; 3];
    point[u_axis.index()] = u;
    point[v_axis.index()] = v;
    point[w_axis.index()] = w;
    point
}

#[derive(Debug, Clone)]
struct MprVolume {
    data: Vec<f32>,
    width: usize,
    height: usize,
    depth: usize,
    row_spacing: f32,
    col_spacing: f32,
    slice_spacing: f32,
    default_wc_ww: (f32, f32),
    background_value: f32,
    origin: [f32; 3],
    column_dir: [f32; 3],
    row_dir: [f32; 3],
    slice_dir: [f32; 3],
    patient_min: [f32; 3],
    patient_max: [f32; 3],
    patient_axis_spacing: [f32; 3],
    /// Per-slice `ImagePositionPatient`, ordered along the stack. Used so
    /// gantry tilt (a sheared stack) is sampled correctly.
    slice_positions: Vec<[f32; 3]>,
    /// Per-slice in-plane row direction (unit), ordered along the stack.
    slice_col_dirs: Vec<[f32; 3]>,
    /// Per-slice in-plane column direction (unit), ordered along the stack.
    slice_row_dirs: Vec<[f32; 3]>,
}

impl MprVolume {
    fn from_images(images: &[LoadedImage], indices: &[usize]) -> Result<Self, String> {
        if indices.len() < 2 {
            return Err("MPR requires at least 2 slices in the selected series".to_string());
        }

        let mut slices: Vec<&LoadedImage> = indices
            .iter()
            .filter_map(|idx| images.get(*idx))
            .collect();

        if slices.len() < 2 {
            return Err("MPR requires at least 2 readable slices in the selected series".to_string());
        }

        let Some(first) = slices.first() else {
            return Err("No slices available for MPR".to_string());
        };

        let (width, height) = first.raw_image.dimensions();
        let first_orientation = first
            .image_orientation_patient
            .ok_or_else(|| "MPR requires ImageOrientationPatient for every slice".to_string())?;
        let column_dir = normalize([
            first_orientation[0],
            first_orientation[1],
            first_orientation[2],
        ])
        .ok_or_else(|| "Invalid ImageOrientationPatient row direction".to_string())?;
        let row_dir = normalize([
            first_orientation[3],
            first_orientation[4],
            first_orientation[5],
        ])
        .ok_or_else(|| "Invalid ImageOrientationPatient column direction".to_string())?;
        if dot(column_dir, row_dir).abs() > 1e-3 {
            return Err("MPR requires orthogonal ImageOrientationPatient vectors".to_string());
        }
        let pixel_spacing = first.pixel_spacing.unwrap_or([1.0, 1.0]);
        let fallback_spacing = first
            .spacing_between_slices
            .or(first.slice_thickness)
            .unwrap_or(1.0)
            .abs()
            .max(0.001);

        slices.sort_by(|a, b| {
            let pos_a = a.slice_position();
            let pos_b = b.slice_position();
            pos_a
                .partial_cmp(&pos_b)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.instance_number.cmp(&b.instance_number))
                .then_with(|| a.filename.cmp(&b.filename))
        });

        let mut volume = Vec::with_capacity(width * height * slices.len());
        let mut min_val = f32::MAX;
        let mut max_val = f32::MIN;
        let mut positions = Vec::with_capacity(slices.len());
        let mut patient_positions = Vec::with_capacity(slices.len());
        let mut slice_positions: Vec<[f32; 3]> = Vec::with_capacity(slices.len());
        let mut slice_col_dirs: Vec<[f32; 3]> = Vec::with_capacity(slices.len());
        let mut slice_row_dirs: Vec<[f32; 3]> = Vec::with_capacity(slices.len());

        for slice in &slices {
            let (slice_width, slice_height) = slice.raw_image.dimensions();
            if slice_width != width || slice_height != height {
                return Err("MPR requires a series where every slice has identical dimensions".to_string());
            }

            if let Some(spacing) = slice.pixel_spacing {
                let same_spacing = (spacing[0] - pixel_spacing[0]).abs() < 1e-3
                    && (spacing[1] - pixel_spacing[1]).abs() < 1e-3;
                if !same_spacing {
                    return Err("MPR requires consistent pixel spacing across the series".to_string());
                }
            }

            let position = slice
                .image_position_patient
                .ok_or_else(|| "MPR requires ImagePositionPatient for every slice".to_string())?;
            let orientation = slice
                .image_orientation_patient
                .ok_or_else(|| "MPR requires ImageOrientationPatient for every slice".to_string())?;
            let slice_column = normalize([orientation[0], orientation[1], orientation[2]])
                .ok_or_else(|| "Invalid ImageOrientationPatient row direction".to_string())?;
            let slice_row = normalize([orientation[3], orientation[4], orientation[5]])
                .ok_or_else(|| "Invalid ImageOrientationPatient column direction".to_string())?;
            if dot(slice_column, column_dir) < 0.999 || dot(slice_row, row_dir) < 0.999 {
                return Err("MPR requires a series with consistent slice orientation".to_string());
            }

            positions.push(slice.slice_position());
            patient_positions.push(position);
            slice_positions.push(position);
            slice_col_dirs.push(slice_column);
            slice_row_dirs.push(slice_row);

            match &slice.raw_image {
                RawImage::Gray8 { data, .. } => {
                    for &value in data {
                        let value = value as f32;
                        min_val = min_val.min(value);
                        max_val = max_val.max(value);
                        volume.push(value);
                    }
                }
                RawImage::Gray16(gray) => {
                    for &value in &gray.data {
                        min_val = min_val.min(value);
                        max_val = max_val.max(value);
                        volume.push(value);
                    }
                }
                RawImage::Rgb8 { .. } => {
                    return Err("MPR is only available for grayscale series".to_string());
                }
            }
        }

        let mut slice_dir = normalize(cross(column_dir, row_dir))
            .ok_or_else(|| "Invalid slice normal derived from ImageOrientationPatient".to_string())?;
        for pair in patient_positions.windows(2) {
            let delta = sub(pair[1], pair[0]);
            if let Some(normalized) = normalize(delta) {
                slice_dir = normalized;
                break;
            }
        }

        let slice_spacing = positions
            .windows(2)
            .filter_map(|pair| {
                let delta = (pair[1] - pair[0]).abs();
                (delta > 1e-3).then_some(delta)
            })
            .reduce(|acc, value| acc + value)
            .map(|sum| sum / (positions.windows(2).filter(|pair| (pair[1] - pair[0]).abs() > 1e-3).count() as f32))
            .unwrap_or(fallback_spacing);

        let default_wc_ww = ((min_val + max_val) / 2.0, (max_val - min_val).max(1.0));
        let origin = patient_positions
            .first()
            .copied()
            .ok_or_else(|| "No slice positions available for MPR".to_string())?;
        let col_step = pixel_spacing[1].abs().max(0.001);
        let row_step = pixel_spacing[0].abs().max(0.001);
        let max_column = width.saturating_sub(1) as f32;
        let max_row = height.saturating_sub(1) as f32;
        // Bounds from the true slice corners, so a tilted (sheared) stack is
        // enclosed correctly.
        let mut patient_min = [f32::MAX; 3];
        let mut patient_max = [f32::MIN; 3];
        for s in 0..slices.len() {
            let base = slice_positions[s];
            for (ci, ri) in [
                (0.0, 0.0),
                (max_column, 0.0),
                (0.0, max_row),
                (max_column, max_row),
            ] {
                let point = add(
                    base,
                    add(
                        scale(slice_col_dirs[s], ci * col_step),
                        scale(slice_row_dirs[s], ri * row_step),
                    ),
                );
                for axis in 0..3 {
                    patient_min[axis] = patient_min[axis].min(point[axis]);
                    patient_max[axis] = patient_max[axis].max(point[axis]);
                }
            }
        }
        // Output spacing for an axis-aligned resample grid. Use the source
        // voxel's projected extent along each patient axis (the sum of the
        // absolute edge projections), so we neither oversample a tilted stack
        // (which made the Z step collapse to the in-plane pixel size) nor
        // undersample an axis-aligned one. For an axis-aligned acquisition this
        // equals the source voxel size along that axis.
        let slice_step = slice_spacing.abs().max(0.001);
        let axis_spacing = |axis: PatientAxis| {
            let unit = axis.unit();
            let projected = col_step * dot(column_dir, unit).abs()
                + row_step * dot(row_dir, unit).abs()
                + slice_step * dot(slice_dir, unit).abs();
            // Guard against a degenerate (all-parallel) geometry: fall back to
            // the finest source spacing so we never pick zero.
            if projected > 0.125 {
                projected
            } else {
                col_step.min(row_step).min(slice_step).max(0.125)
            }
        };
        let patient_axis_spacing = [
            axis_spacing(PatientAxis::X),
            axis_spacing(PatientAxis::Y),
            axis_spacing(PatientAxis::Z),
        ];

        Ok(Self {
            data: volume,
            width,
            height,
            depth: slices.len(),
            row_spacing: pixel_spacing[0].abs().max(0.001),
            col_spacing: pixel_spacing[1].abs().max(0.001),
            slice_spacing: slice_spacing.abs().max(0.001),
            default_wc_ww,
            background_value: min_val,
            origin,
            column_dir,
            row_dir,
            slice_dir,
            patient_min,
            patient_max,
            patient_axis_spacing,
            slice_positions,
            slice_col_dirs,
            slice_row_dirs,
        })
    }

    fn axis_extent(&self, axis: PatientAxis) -> f32 {
        let idx = axis.index();
        (self.patient_max[idx] - self.patient_min[idx]).max(0.0)
    }

    fn axis_spacing(&self, axis: PatientAxis) -> f32 {
        self.patient_axis_spacing[axis.index()]
    }

    fn axis_samples(&self, axis: PatientAxis) -> usize {
        ((self.axis_extent(axis) / self.axis_spacing(axis)).round() as usize)
            .saturating_add(1)
            .max(1)
    }

    fn axis_coordinate(&self, axis: PatientAxis, index: usize) -> f32 {
        let idx = axis.index();
        let coordinate = self.patient_min[idx] + index as f32 * self.axis_spacing(axis);
        coordinate.min(self.patient_max[idx])
    }

    /// Fractional slice index for a patient point along the stack axis,
    /// measured from the first slice's position.
    fn slice_coordinate(&self, patient_point: [f32; 3]) -> f32 {
        let d = sub(patient_point, self.slice_positions[0]);
        dot(d, self.slice_dir) / self.slice_spacing.max(0.001)
    }

    /// Sample one slice at in-plane `(column, row)` for the given slice index.
    fn sample_slice_inplane(&self, s: usize, column: f32, row: f32) -> f32 {
        let max_c = self.width.saturating_sub(1) as f32;
        let max_r = self.height.saturating_sub(1) as f32;
        if column < 0.0 || row < 0.0 || column > max_c || row > max_r {
            return self.background_value;
        }
        let c0 = column.floor() as usize;
        let r0 = row.floor() as usize;
        let c1 = (c0 + 1).min(self.width.saturating_sub(1));
        let r1 = (r0 + 1).min(self.height.saturating_sub(1));
        let dc = column - c0 as f32;
        let dr = row - r0 as f32;
        let idx = |c: usize, r: usize| s * self.width * self.height + r * self.width + c;
        let v00 = self.data[idx(c0, r0)] * (1.0 - dc) + self.data[idx(c1, r0)] * dc;
        let v10 = self.data[idx(c0, r1)] * (1.0 - dc) + self.data[idx(c1, r1)] * dc;
        v00 * (1.0 - dr) + v10 * dr
    }

    /// Sample the volume at a patient-space point.
    ///
    /// Uses each slice's own `ImagePositionPatient` and in-plane axes, so a
    /// sheared stack (gantry tilt) is handled correctly rather than assuming a
    /// rectangular grid.
    fn sample_trilinear(&self, patient_point: [f32; 3]) -> f32 {
        let slice = self.slice_coordinate(patient_point);
        let max_s = self.depth.saturating_sub(1) as f32;
        if slice < -0.5 || slice > max_s + 0.5 {
            return self.background_value;
        }
        let s0 = slice.floor().max(0.0) as usize;
        let s1 = (s0 + 1).min(self.depth.saturating_sub(1));
        let ds = (slice - s0 as f32).clamp(0.0, 1.0);

        let inplane = |s: usize| -> f32 {
            let origin = self.slice_positions[s];
            let delta = sub(patient_point, origin);
            // DICOM: the first IOP triplet is the direction of increasing column
            // index (stored here as `slice_col_dirs`), the second of increasing row
            // index (`slice_row_dirs`). The volume x-axis is the column index and the
            // y-axis the row index.
            let column = dot(delta, self.slice_col_dirs[s]) / self.col_spacing.max(0.001);
            let row = dot(delta, self.slice_row_dirs[s]) / self.row_spacing.max(0.001);
            self.sample_slice_inplane(s, column, row)
        };

        if s0 == s1 {
            return inplane(s0);
        }
        inplane(s0) * (1.0 - ds) + inplane(s1) * ds
    }

    fn plane_len(&self, plane: MprPlane) -> usize {
        let (_, _, w_axis) = plane.patient_axes();
        self.axis_samples(w_axis)
    }

    fn plane_dimensions(&self, plane: MprPlane) -> (usize, usize) {
        let (u_axis, v_axis, _) = plane.patient_axes();
        (self.axis_samples(u_axis), self.axis_samples(v_axis))
    }

    fn physical_size(&self, plane: MprPlane) -> Vec2 {
        let (u_axis, v_axis, _) = plane.patient_axes();
        egui::vec2(self.axis_extent(u_axis), self.axis_extent(v_axis))
    }

    fn extract_plane(&self, plane: MprPlane, index: usize) -> Vec<f32> {
        let (u_axis, v_axis, w_axis) = plane.patient_axes();
        let width = self.axis_samples(u_axis);
        let height = self.axis_samples(v_axis);
        let slice_index = index.min(self.axis_samples(w_axis).saturating_sub(1));
        let w_coord = self.axis_coordinate(w_axis, slice_index);
        let mut out = vec![self.background_value; width * height];
        // Flip by reversing the sample order (not by negating coordinates, which
        // would move the sample outside the volume).
        let u_flip = plane.u_sign() < 0.0;
        let v_flip = plane.v_sign() < 0.0;

        for row_idx in 0..height {
            let v_sample = if v_flip { height - 1 - row_idx } else { row_idx };
            let v_coord = self.axis_coordinate(v_axis, v_sample);
            for col_idx in 0..width {
                let u_sample = if u_flip { width - 1 - col_idx } else { col_idx };
                let u_coord = self.axis_coordinate(u_axis, u_sample);
                let patient_point = axis_aligned_patient_point(
                    u_axis,
                    v_axis,
                    w_axis,
                    u_coord,
                    v_coord,
                    w_coord,
                );
                out[row_idx * width + col_idx] = self.sample_trilinear(patient_point);
            }
        }

        out
    }
}

fn scalar_to_rgba(data: &[f32], wc: f32, ww: f32) -> Vec<u8> {
    let mut rgba = vec![0u8; data.len() * 4];
    let lo = wc - ww / 2.0;
    let hi = wc + ww / 2.0;
    let denom = (hi - lo).max(f32::EPSILON);
    for (i, &value) in data.iter().enumerate() {
        let norm = ((value - lo) / denom).clamp(0.0, 1.0);
        let byte = (norm * 255.0) as u8;
        rgba[i * 4] = byte;
        rgba[i * 4 + 1] = byte;
        rgba[i * 4 + 2] = byte;
        rgba[i * 4 + 3] = 255;
    }
    rgba
}

/// Blend a translucent red tint into the RGBA buffer where `flags` is true, so
/// the pixels that defacing would blank stand out.
fn tint_removed(rgba: &mut [u8], flags: &[bool]) {
    let n = (rgba.len() / 4).min(flags.len());
    for i in 0..n {
        if flags[i] {
            let p = i * 4;
            // 50% toward red (255, 40, 40).
            rgba[p] = ((rgba[p] as u16 + 255) / 2) as u8;
            rgba[p + 1] = (rgba[p + 1] as u16 / 2 + 20) as u8;
            rgba[p + 2] = (rgba[p + 2] as u16 / 2 + 20) as u8;
            rgba[p + 3] = 255;
        }
    }
}

fn make_thumbnail(raw: &RawImage, max_dim: usize, wc: f32, ww: f32) -> (usize, usize, Vec<u8>) {
    let (w, h) = raw.dimensions();
    let scale = if w == 0 || h == 0 {
        1.0
    } else {
        (max_dim as f32 / (w.max(h) as f32)).min(1.0)
    };
    let tw = (w as f32 * scale).max(1.0) as usize;
    let th = (h as f32 * scale).max(1.0) as usize;
    let mut out = vec![0u8; tw * th * 4];

    for ty in 0..th {
        for tx in 0..tw {
            let sx = ((tx as f32 + 0.5) / (tw as f32) * (w as f32)).floor().min((w - 1) as f32) as usize;
            let sy = ((ty as f32 + 0.5) / (th as f32) * (h as f32)).floor().min((h - 1) as f32) as usize;
            let dst_idx = (ty * tw + tx) * 4;
            match raw {
                RawImage::Gray8 { data, width, .. } => {
                    let v = data[sy * width + sx];
                    out[dst_idx] = v;
                    out[dst_idx + 1] = v;
                    out[dst_idx + 2] = v;
                    out[dst_idx + 3] = 255;
                }
                RawImage::Gray16(g) => {
                    let val = g.data[sy * g.width + sx];
                    let lo = wc - ww / 2.0;
                    let hi = wc + ww / 2.0;
                    let norm = ((val - lo) / (hi - lo).max(f32::EPSILON)).clamp(0.0, 1.0);
                    let byte = (norm * 255.0) as u8;
                    out[dst_idx] = byte;
                    out[dst_idx + 1] = byte;
                    out[dst_idx + 2] = byte;
                    out[dst_idx + 3] = 255;
                }
                RawImage::Rgb8 { data, width, .. } => {
                    let base = (sy * width + sx) * 3;
                    out[dst_idx] = data[base];
                    out[dst_idx + 1] = data[base + 1];
                    out[dst_idx + 2] = data[base + 2];
                    out[dst_idx + 3] = 255;
                }
            }
        }
    }

    (tw, th, out)
}


#[derive(Clone)]
enum RawImage {
    Gray8 {
        data: Vec<u8>,
        width: usize,
        height: usize,
    },
    Gray16(Gray16),
    Rgb8 {
        data: Vec<u8>,
        width: usize,
        height: usize,
    },
}

impl RawImage {
    fn dimensions(&self) -> (usize, usize) {
        match self {
            RawImage::Gray8 { width, height, .. } => (*width, *height),
            RawImage::Gray16(g) => (g.width, g.height),
            RawImage::Rgb8 { width, height, .. } => (*width, *height),
        }
    }

    fn is_grayscale16(&self) -> bool {
        matches!(self, RawImage::Gray16(_))
    }

    /// Produce an RGBA byte buffer applying window centre/width for 16-bit images.
    fn to_rgba(&self, wc: f32, ww: f32) -> Vec<u8> {
        match self {
            RawImage::Gray8 { data, width, height } => {
                let n = width * height;
                let mut rgba = vec![0u8; n * 4];
                for (i, &v) in data.iter().enumerate() {
                    rgba[i * 4] = v;
                    rgba[i * 4 + 1] = v;
                    rgba[i * 4 + 2] = v;
                    rgba[i * 4 + 3] = 255;
                }
                rgba
            }
            RawImage::Gray16(g) => scalar_to_rgba(&g.data, wc, ww),
            RawImage::Rgb8 { data, width, height } => {
                let n = width * height;
                let mut rgba = vec![0u8; n * 4];
                for i in 0..n {
                    rgba[i * 4] = data[i * 3];
                    rgba[i * 4 + 1] = data[i * 3 + 1];
                    rgba[i * 4 + 2] = data[i * 3 + 2];
                    rgba[i * 4 + 3] = 255;
                }
                rgba
            }
        }
    }
}

/// One loaded DICOM image with its decoded pixels and metadata.
#[derive(Clone)]
struct LoadedImage {
    raw_image: RawImage,
    metadata: Vec<(String, String)>,
    filename: String,
    series_uid: String,
    series_label: String,
    instance_number: Option<i32>,
    default_wc_ww: Option<(f32, f32)>,
    /// Small RGBA thumbnail (width, height, rgba bytes)
    thumbnail: Option<(usize, usize, Vec<u8>)>,
    pixel_spacing: Option<[f32; 2]>,
    slice_thickness: Option<f32>,
    spacing_between_slices: Option<f32>,
    image_position_patient: Option<[f32; 3]>,
    image_orientation_patient: Option<[f32; 6]>,
    study_uid: Option<String>,
}

impl LoadedImage {
    fn physical_size(&self) -> Vec2 {
        let (width, height) = self.raw_image.dimensions();
        if let Some([row_spacing, col_spacing]) = self.pixel_spacing {
            egui::vec2(
                width as f32 * col_spacing.abs().max(0.001),
                height as f32 * row_spacing.abs().max(0.001),
            )
        } else {
            egui::vec2(width as f32, height as f32)
        }
    }

    fn slice_position(&self) -> f32 {
        if let (Some(position), Some(orientation)) = (
            self.image_position_patient,
            self.image_orientation_patient,
        ) {
            let column = [orientation[0], orientation[1], orientation[2]];
            let row = [orientation[3], orientation[4], orientation[5]];
            let normal = cross(column, row);
            let normal_mag = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            if normal_mag > 1e-6 {
                return position[0] * normal[0] + position[1] * normal[1] + position[2] * normal[2];
            }
        }

        self.instance_number.map(|value| value as f32).unwrap_or(0.0)
    }
}

#[derive(Clone)]
struct SeriesGroup {
    uid: String,
    label: String,
    image_indices: Vec<usize>,
}

/// Per-viewport UI state so multiple viewports can be added later.
struct ViewportState {
    current_series: usize,
    current_stack_slice: usize,
    current_axial_slice: usize,
    current_coronal_slice: usize,
    current_sagittal_slice: usize,
    view_mode: ViewMode,
    mpr_plane: MprPlane,
    /// Sync group: viewports sharing the same `Some(group)` stay in lockstep
    /// when navigated. `None` means this viewport is independent.
    sync_group: Option<u32>,
    displayed_physical_size: Option<Vec2>,
    scale_by_physical: bool,
    window_center: f32,
    window_width: f32,
    wl_dirty: bool,
    zoom: f32,
    rotation_degrees: f32,
    rotation_drag_last_pos: Option<egui::Pos2>,
    /// Screen position of the cursor when a both-buttons zoom drag started.
    /// Held fixed for the duration of the drag so the image point under the
    /// cursor stays locked while the pointer moves.
    zoom_drag_anchor: Option<egui::Pos2>,
    /// Zoom level when the both-buttons zoom drag started. The current zoom is
    /// derived from the total pointer displacement from `zoom_drag_anchor`
    /// (rather than compounding per-frame deltas) so the zoom is stable and
    /// independent of frame timing.
    zoom_drag_start_zoom: f32,
    pan: Vec2,
}

impl ViewportState {
    /// Current slice value for this viewport's MPR plane.
    fn mpr_slice_value(&self) -> usize {
        match self.mpr_plane {
            MprPlane::Axial => self.current_axial_slice,
            MprPlane::Coronal => self.current_coronal_slice,
            MprPlane::Sagittal => self.current_sagittal_slice,
        }
    }

    fn set_mpr_slice_value(&mut self, value: usize) {
        match self.mpr_plane {
            MprPlane::Axial => self.current_axial_slice = value,
            MprPlane::Coronal => self.current_coronal_slice = value,
            MprPlane::Sagittal => self.current_sagittal_slice = value,
        }
    }
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            current_series: 0,
            current_stack_slice: 0,
            current_axial_slice: 0,
            current_coronal_slice: 0,
            current_sagittal_slice: 0,
            view_mode: ViewMode::Stack,
            mpr_plane: MprPlane::Axial,
            sync_group: None,
            displayed_physical_size: None,
            scale_by_physical: true,
            window_center: 0.0,
            window_width: 1.0,
            wl_dirty: false,
            zoom: 1.0,
            rotation_degrees: 0.0,
            rotation_drag_last_pos: None,
            zoom_drag_anchor: None,
            zoom_drag_start_zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

/// Message sent from the background loading thread to the UI thread.
enum LoadMsg {
    /// One file decoded successfully.
    Image(LoadedImage),
    /// A file failed to decode.
    Error(String),
}

struct LoadingState {
    rx: mpsc::Receiver<LoadMsg>,
    total: usize,
    received: usize,
    current_filename: String,
}

fn has_supported_dicom_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let e = ext.to_ascii_lowercase();
            e == "dcm" || e == "dicom"
        })
        .unwrap_or(false)
}

fn has_dicom_preamble(path: &std::path::Path) -> bool {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };

    let mut header = [0u8; 132];
    if file.read_exact(&mut header).is_err() {
        return false;
    }

    &header[128..132] == b"DICM"
}

fn is_supported_dicom_file(path: &std::path::Path) -> bool {
    if has_supported_dicom_extension(path) {
        return true;
    }

    // For extensionless files, check for DICOM preamble first, then fall back to parser probe
    // to support valid DICOM files without a .dcm/.dicom suffix.
    if path.extension().is_none() {
        return has_dicom_preamble(path) || open_file(path).is_ok();
    }

    false
}

fn collect_dicom_files_recursively(inputs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = inputs;

    while let Some(path) = stack.pop() {
        if path.is_file() {
            if is_supported_dicom_file(&path) {
                out.push(path);
            }
            continue;
        }

        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    stack.push(entry.path());
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Decode a single DICOM file into a `LoadedImage`. Runs on a worker thread.
fn decode_single_file(path: &PathBuf) -> Result<LoadedImage, String> {
    let obj = open_file(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let mut metadata = Vec::new();
    for (tag, label) in METADATA_TAGS {
        if let Ok(elem) = obj.element(*tag) {
            if let Ok(val) = elem.to_str() {
                let val = val.trim().to_string();
                if !val.is_empty() {
                    metadata.push((label.to_string(), val));
                }
            }
        }
    }

    let get_str = |tag: Tag| -> Option<String> {
        obj.element(tag)
            .ok()?
            .to_str()
            .ok()
            .map(|s| s.trim().to_string())
    };
    let get_f32 = |tag: Tag| -> Option<f32> {
        get_str(tag).and_then(|s| {
            s.split('\\').next().and_then(|v| v.trim().parse::<f32>().ok())
        })
    };
    let get_i32 = |tag: Tag| -> Option<i32> {
        get_str(tag).and_then(|s| {
            s.split('\\').next().and_then(|v| v.trim().parse::<i32>().ok())
        })
    };
    let get_multi_f32 = |tag: Tag, expected: usize| -> Option<Vec<f32>> {
        let values = get_str(tag)?
            .split('\\')
            .filter_map(|value| value.trim().parse::<f32>().ok())
            .collect::<Vec<_>>();
        (values.len() >= expected).then_some(values)
    };

    let series_uid = get_str(Tag(0x0020, 0x000E)).unwrap_or_else(|| "NO_SERIES_UID".to_string());
    let series_desc = get_str(Tag(0x0008, 0x103E)).unwrap_or_else(|| "(no description)".to_string());
    let series_number = get_str(Tag(0x0020, 0x0011));
    let instance_number = get_i32(Tag(0x0020, 0x0013));
    let series_label = match series_number {
        Some(n) if !n.is_empty() => format!("Series {} - {}", n, series_desc),
        _ => format!("Series - {}", series_desc),
    };

    let study_uid = get_str(Tag(0x0020, 0x000D));

    let rescale_intercept = get_f32(Tag(0x0028, 0x1052)).unwrap_or(0.0);
    let rescale_slope = get_f32(Tag(0x0028, 0x1053)).unwrap_or(1.0);
    let wc_dicom = get_f32(Tag(0x0028, 0x1050));
    let ww_dicom = get_f32(Tag(0x0028, 0x1051));
    let pixel_spacing = get_multi_f32(Tag(0x0028, 0x0030), 2).map(|values| [values[0], values[1]]);
    let slice_thickness = get_f32(Tag(0x0018, 0x0050));
    let spacing_between_slices = get_f32(Tag(0x0018, 0x0088));
    let image_position_patient = get_multi_f32(Tag(0x0020, 0x0032), 3)
        .map(|values| [values[0], values[1], values[2]]);
    let image_orientation_patient = get_multi_f32(Tag(0x0020, 0x0037), 6)
        .map(|values| [values[0], values[1], values[2], values[3], values[4], values[5]]);

    let pixel_data = obj
        .decode_pixel_data()
        .map_err(|e| format!("Failed to decode pixel data in {}: {}", path.display(), e))?;

    let rows = pixel_data.rows() as usize;
    let cols = pixel_data.columns() as usize;
    let bits = pixel_data.bits_allocated();
    let samples = pixel_data.samples_per_pixel();
    let signed = matches!(
        pixel_data.pixel_representation(),
        dicom_pixeldata::PixelRepresentation::Signed
    );
    let bytes: &[u8] = pixel_data.data();

    let raw_image = match (bits, samples) {
        (8, 1) => {
            if bytes.len() < rows * cols {
                return Err("Pixel data buffer is too short".to_string());
            }
            RawImage::Gray8 {
                data: bytes[..rows * cols].to_vec(),
                width: cols,
                height: rows,
            }
        }
        (16, 1) => {
            let expected = rows * cols * 2;
            if bytes.len() < expected {
                return Err(format!(
                    "Pixel data buffer too short: {} bytes, expected {}",
                    bytes.len(),
                    expected
                ));
            }
            let data: Vec<f32> = bytes
                .chunks_exact(2)
                .take(rows * cols)
                .map(|c| {
                    let raw_u16 = u16::from_le_bytes([c[0], c[1]]);
                    let raw_val = if signed {
                        (raw_u16 as i16) as f32
                    } else {
                        raw_u16 as f32
                    };
                    raw_val * rescale_slope + rescale_intercept
                })
                .collect();
            RawImage::Gray16(Gray16 {
                data,
                width: cols,
                height: rows,
            })
        }
        (8, 3) => {
            let expected = rows * cols * 3;
            if bytes.len() < expected {
                return Err("Pixel data buffer is too short".to_string());
            }
            RawImage::Rgb8 {
                data: bytes[..expected].to_vec(),
                width: cols,
                height: rows,
            }
        }
        _ => {
            return Err(format!(
                "Unsupported pixel format: {} bpp, {} samples per pixel",
                bits, samples
            ));
        }
    };

    let default_wc_ww = if let RawImage::Gray16(ref g) = raw_image {
        let (wc, ww) = if let (Some(wc), Some(ww)) = (wc_dicom, ww_dicom) {
            (wc, ww)
        } else {
            let min = g.data.iter().cloned().fold(f32::MAX, f32::min);
            let max = g.data.iter().cloned().fold(f32::MIN, f32::max);
            ((min + max) / 2.0, (max - min).max(1.0))
        };
        Some((wc, ww))
    } else {
        None
    };

    // Create a small thumbnail for UI (use default window/level for 16-bit images if available)
    let thumbnail = {
        let (tw, th, rgba) = if let Some((wc, ww)) = default_wc_ww {
            make_thumbnail(&raw_image, 64, wc, ww)
        } else {
            make_thumbnail(&raw_image, 64, 0.0, 1.0)
        };
        Some((tw, th, rgba))
    };

    Ok(LoadedImage {
        raw_image,
        metadata,
        filename,
        series_uid,
        series_label,
        instance_number,
        default_wc_ww,
        thumbnail,
        pixel_spacing,
        slice_thickness,
        spacing_between_slices,
        image_position_patient,
        image_orientation_patient,
        study_uid,
    })
}

/// Geometry for defacing a decoded series, mapping native voxels to patient
/// space and back.
///
/// The defacing volume uses the acquisition's own (orthonormal) frame
/// `[col_dir, row_dir, slice_dir]`; this matches `diface-rs`'s `series.rs` and
/// handles oblique and gantry-tilted stacks, since the geometric backend works
/// in patient space and is robust to the residual shear.
struct DefaceGeometry {
    /// Volume whose z-axis follows the sorted slice order.
    volume: DifaceVolume,
    /// Native image indices in slice order.
    order: Vec<usize>,
    /// Native in-plane dimensions (width = columns, height = rows).
    width: usize,
    height: usize,
}

impl DefaceGeometry {
    /// Is native voxel `(x, y, z)` removed by `mask`? The volume is built on the
    /// native grid, so the mapping is the identity.
    fn is_removed_native(
        &self,
        x: usize,
        y: usize,
        z: usize,
        mask: &diface_rs::volume::Mask,
    ) -> bool {
        if x >= self.width || y >= self.height || z >= self.order.len() {
            return false;
        }
        mask.is_removed(x, y, z)
    }
}

/// A defacing removal preview for one series, usable in both Stack and MPR.
#[derive(Clone)]
struct DefacePreview {
    /// Base series UID this preview belongs to.
    uid: String,
    /// 3D mask in the series volume grid `[nx, ny, nz]`, indexed
    /// `(z*ny + y)*nx + x` (matching `MprVolume` and the deface volume).
    volume_flags: Vec<bool>,
    dims: [usize; 3],
    /// Per native image index, the in-plane flags (for Stack viewports).
    per_image: HashMap<usize, Vec<bool>>,
    /// Per-voxel segmentation label (0 none, 1 head, 2 brain, 3 vault,
    /// 4 cavity), when a segmentation preview is available. Same grid as
    /// `volume_flags`.
    volume_labels: Option<Vec<u8>>,
    /// Per native image index, the in-plane labels (for Stack viewports).
    per_image_labels: Option<HashMap<usize, Vec<u8>>>,
}

impl DefacePreview {
    #[inline]
    fn is_removed(&self, x: usize, y: usize, z: usize) -> bool {
        if x >= self.dims[0] || y >= self.dims[1] || z >= self.dims[2] {
            return false;
        }
        self.volume_flags[(z * self.dims[1] + y) * self.dims[0] + x]
    }

    /// In-plane removal flags for an MPR plane, matching `MprVolume::extract_plane`
    /// (same axis mapping and u/v flips), so the preview lines up with the
    /// displayed MPR image.
    fn plane_flags(&self, plane: MprPlane, index: usize) -> Vec<bool> {
        let (u_axis, v_axis, w_axis) = plane.patient_axes();
        let (ui, vi, wi) = (u_axis.index(), v_axis.index(), w_axis.index());
        let width = self.dims[ui];
        let height = self.dims[vi];
        let w_len = self.dims[wi];
        let w_index = index.min(w_len.saturating_sub(1));
        let u_flip = plane.u_sign() < 0.0;
        let v_flip = plane.v_sign() < 0.0;

        let mut flags = vec![false; width * height];
        for row in 0..height {
            let v_sample = if v_flip { height - 1 - row } else { row };
            for col in 0..width {
                let u_sample = if u_flip { width - 1 - col } else { col };
                let mut coord = [0usize; 3];
                coord[ui] = u_sample.min(self.dims[ui].saturating_sub(1));
                coord[vi] = v_sample.min(self.dims[vi].saturating_sub(1));
                coord[wi] = w_index;
                flags[row * width + col] = self.is_removed(coord[0], coord[1], coord[2]);
            }
        }
        flags
    }

    /// In-plane segmentation labels for an MPR plane (see `plane_flags` for the
    /// axis mapping). Returns `None` when no segmentation preview is available.
    fn plane_labels(&self, plane: MprPlane, index: usize) -> Option<Vec<u8>> {
        let labels = self.volume_labels.as_ref()?;
        let (u_axis, v_axis, w_axis) = plane.patient_axes();
        let (ui, vi, wi) = (u_axis.index(), v_axis.index(), w_axis.index());
        let width = self.dims[ui];
        let height = self.dims[vi];
        let w_len = self.dims[wi];
        let w_index = index.min(w_len.saturating_sub(1));
        let u_flip = plane.u_sign() < 0.0;
        let v_flip = plane.v_sign() < 0.0;

        let mut out = vec![0u8; width * height];
        for row in 0..height {
            let v_sample = if v_flip { height - 1 - row } else { row };
            for col in 0..width {
                let u_sample = if u_flip { width - 1 - col } else { col };
                let mut coord = [0usize; 3];
                coord[ui] = u_sample.min(self.dims[ui].saturating_sub(1));
                coord[vi] = v_sample.min(self.dims[vi].saturating_sub(1));
                coord[wi] = w_index;
                out[row * width + col] =
                    labels[(coord[2] * self.dims[1] + coord[1]) * self.dims[0] + coord[0]];
            }
        }
        Some(out)
    }
}

/// Blend segmentation-area colours into an RGBA buffer.
///
/// Labels: 1 = head (blue), 2 = brain (green), 3 = vault (orange),
/// 4 = intracranial cavity (cyan).
fn tint_segmentation(rgba: &mut [u8], labels: &[u8]) {
    let n = (rgba.len() / 4).min(labels.len());
    for i in 0..n {
        let p = i * 4;
        let (r, g, b) = match labels[i] {
            1 => (60u8, 90, 200),
            2 => (60, 200, 90),
            3 => (230, 150, 40),
            4 => (80, 200, 210),
            _ => continue,
        };
        // 35% blend toward the area colour.
        let blend = |src: u8, dst: u8| ((src as u16 * 65 + dst as u16 * 35) / 100) as u8;
        rgba[p] = blend(rgba[p], r);
        rgba[p + 1] = blend(rgba[p + 1], g);
        rgba[p + 2] = blend(rgba[p + 2], b);
    }
}

/// Build the defacing geometry for the given series images.
fn build_deface_volume(
    images: &[LoadedImage],
    indices: &[usize],
) -> Result<DefaceGeometry, String> {
    let mut order: Vec<usize> = indices
        .iter()
        .copied()
        .filter(|i| images.get(*i).is_some())
        .collect();
    if order.len() < 2 {
        return Err("Defacing requires at least 2 slices".to_string());
    }

    // All candidates must share dimensions, like `diface_rs::series::load_series`.
    let (width, height) = images[order[0]].raw_image.dimensions();
    for &i in &order {
        if images[i].raw_image.dimensions() != (width, height) {
            return Err("Defacing requires slices with identical dimensions".to_string());
        }
    }

    // Group slices by ImageOrientationPatient and keep the largest
    // orientation-consistent group, mirroring `diface_rs::series::load_series`.
    // A multi-orientation series (e.g. localisers saved under one
    // SeriesInstanceUID) is not a rigid volume; the in-viewer deface must pick
    // the same subset the CLI/uploader would, so the preview matches the output.
    const IOP_TOL: f32 = 1e-3;
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &order {
        let iop = images[i].image_orientation_patient;
        let mut matched = false;
        for members in groups.iter_mut() {
            let Some(reference) = images[members[0]].image_orientation_patient else {
                continue;
            };
            let Some(iop) = iop else { continue };
            if (0..6).all(|k| (iop[k] - reference[k]).abs() <= IOP_TOL) {
                members.push(i);
                matched = true;
                break;
            }
        }
        if !matched {
            groups.push(vec![i]);
        }
    }
    groups.sort_by(|a, b| b.len().cmp(&a.len()));
    order = groups.into_iter().next().unwrap_or_default();
    if order.len() < 2 {
        return Err(
            "Defacing requires at least 2 slices with a consistent orientation".to_string(),
        );
    }

    let first = &images[order[0]];
    let orientation = first
        .image_orientation_patient
        .ok_or_else(|| "Defacing requires ImageOrientationPatient".to_string())?;
    // DICOM IOP: first triplet = direction of increasing column index (the
    // "row" vector), second = increasing row index.
    let col_dir = normalize([orientation[0], orientation[1], orientation[2]])
        .ok_or_else(|| "Invalid ImageOrientationPatient row direction".to_string())?;
    let row_dir = normalize([orientation[3], orientation[4], orientation[5]])
        .ok_or_else(|| "Defacing requires orthogonal ImageOrientationPatient".to_string())?;
    if dot(col_dir, row_dir).abs() > 1e-3 {
        return Err("Defacing requires orthogonal ImageOrientationPatient vectors".to_string());
    }
    // Use the IOP cross product for the slice axis so the frame is orthonormal;
    // the position delta is not perpendicular to the tilted rows.
    let slice_dir = normalize(cross(col_dir, row_dir))
        .ok_or_else(|| "Invalid slice normal derived from ImageOrientationPatient".to_string())?;

    let pixel_spacing = first.pixel_spacing.unwrap_or([1.0, 1.0]);
    let fallback_spacing = first
        .spacing_between_slices
        .or(first.slice_thickness)
        .unwrap_or(1.0)
        .abs()
        .max(0.001);

    order.sort_by(|a, b| {
        let ia = &images[*a];
        let ib = &images[*b];
        ia.slice_position()
            .partial_cmp(&ib.slice_position())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| ia.instance_number.cmp(&ib.instance_number))
            .then_with(|| ia.filename.cmp(&ib.filename))
    });

    let mut data = Vec::with_capacity(width * height * order.len());
    let mut positions = Vec::with_capacity(order.len());
    let mut patient_positions = Vec::with_capacity(order.len());
    for &i in &order {
        let img = &images[i];
        if img.raw_image.dimensions() != (width, height) {
            return Err("Defacing requires slices with identical dimensions".to_string());
        }
        if let Some(ps) = img.pixel_spacing {
            if (ps[0] - pixel_spacing[0]).abs() > 1e-3
                || (ps[1] - pixel_spacing[1]).abs() > 1e-3
            {
                return Err("Defacing requires consistent pixel spacing".to_string());
            }
        }
        match &img.raw_image {
            RawImage::Gray16(g) => data.extend_from_slice(&g.data),
            _ => return Err("Defacing is only available for grayscale series".to_string()),
        }
        positions.push(img.slice_position());
        patient_positions.push(
            img.image_position_patient
                .ok_or_else(|| "Defacing requires ImagePositionPatient".to_string())?,
        );
    }

    // Slice spacing = mean projection of consecutive positions onto the normal.
    let slice_spacing = {
        let deltas: Vec<f32> = positions
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .filter(|d| *d > 1e-3)
            .collect();
        if deltas.is_empty() {
            fallback_spacing
        } else {
            deltas.iter().sum::<f32>() / deltas.len() as f32
        }
    };

    let modality = first
        .metadata
        .iter()
        .find(|(label, _)| label == "Modality")
        .map(|(_, value)| value.trim().to_uppercase())
        .unwrap_or_default();
    let to64 = |v: f32| v as f64;
    let to3 = |v: [f32; 3]| [v[0] as f64, v[1] as f64, v[2] as f64];

    let nz = order.len();
    let volume = DifaceVolume {
        data,
        dims: [width, height, nz],
        spacing: [
            to64(pixel_spacing[1].abs().max(0.001)),
            to64(pixel_spacing[0].abs().max(0.001)),
            to64(slice_spacing.abs().max(0.001)),
        ],
        origin: to3(patient_positions[0]),
        dir: [to3(col_dir), to3(row_dir), to3(slice_dir)],
        rescale: vec![(1.0, 0.0); nz],
        modality,
    };

    Ok(DefaceGeometry {
        volume,
        order,
        width,
        height,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefaceBackendSel {
    Geometric,
    Atlas,
    Segmentation,
}

impl DefaceBackendSel {
    fn name(self) -> &'static str {
        match self {
            DefaceBackendSel::Geometric => "Geometric",
            DefaceBackendSel::Atlas => "Atlas",
            DefaceBackendSel::Segmentation => "Segmentation",
        }
    }
}

struct DicomViewApp {
    /// A queued load: the paths plus whether the open should be recorded in the
    /// recent-folders history. Programmatic loads (CLI arguments, `diface --view`,
    /// tests) pass `false`; only user-initiated opens are remembered.
    pending_load: Option<(Vec<PathBuf>, bool)>,
    images: Vec<LoadedImage>,
    series_groups: Vec<SeriesGroup>,
    // Per-viewport states. Default to a single viewport.
    viewports: Vec<ViewportState>,
    active_viewport: usize,
    // One texture handle per viewport (None when not yet built)
    viewport_textures: Vec<Option<egui::TextureHandle>>,
    /// Rectangle each viewport's image is drawn into (filled in during the
    /// draw pass), used to place per-viewport overlay controls.
    viewport_image_rects: Vec<Option<egui::Rect>>,
    /// One MPR volume per series, so each series (original and defaced
    /// copies alike) has its own volume and MPR planes never get mixed.
    mpr_volumes: Vec<Option<MprVolume>>,
    /// Per-series MPR build error, for the toolbar message.
    mpr_errors: Vec<Option<String>>,
    // Recently opened folders (persisted to disk)
    recent_folders: Vec<PathBuf>,
    texture: Option<egui::TextureHandle>,
    thumbnail_textures: HashMap<String, egui::TextureHandle>,
    zoom: f32,
    rotation_degrees: f32,
    rotation_drag_last_pos: Option<egui::Pos2>,
    wheel_slice_accumulator: f32,
    /// Which viewport the wheel accumulator currently belongs to (so a hover
    /// change resets partial wheel movement).
    wheel_owner: Option<usize>,
    pan: Vec2,
    error: Option<String>,
    show_metadata: bool,
    files_hovered: bool,
    loading: Option<LoadingState>,
    /// Status of the most recent in-memory defacing action.
    deface_message: Option<String>,
    /// Manual defacing controls (orientation yaw, anterior depth, extent),
    /// applied on top of the automatic estimate.
    deface_params: GeometricParams,
    /// Which defacing backend the viewer uses.
    deface_backend: DefaceBackendSel,
    /// The atlas brain template (built-in synthetic unless loaded).
    deface_atlas: diface_rs::Atlas,
    /// Whether the manual defacing panel is expanded.
    deface_panel_open: bool,
    /// Show the segmentation areas (head/brain/skull) in the preview overlay.
    deface_show_segmentation: bool,
    /// Whether the removal preview is enabled (default on). When on, the preview
    /// is (re)computed automatically for the active base series.
    deface_preview_on: bool,
    /// What the segmentation backend removes (face cut or external soft tissue).
    deface_seg_region: diface_rs::MaskRegion,
    /// Deflesh: keep external tissue posterior of brain centre + this (mm).
    deface_deflesh_posterior_mm: f64,
    /// Live removal-mask preview (3D + per-image), tinted in viewports that
    /// show the base series (Stack and MPR). `None` when preview is off.
    deface_preview: Option<DefacePreview>,
    /// Viewports whose inline window/level editor is open (toggled by
    /// double-clicking the W/L overlay).
    wl_editor_open: Vec<bool>,
    /// Last observed navigation state of each viewport, used to detect which
    /// viewport was navigated and propagate it (within its sync group) in
    /// `autosync_active`.
    last_navs: Vec<NavSnapshot>,
}

/// Immutable snapshot of a viewport's navigation state.
#[derive(Clone, Copy, PartialEq, Eq)]
struct NavSnapshot {
    series: usize,
    stack: usize,
    axial: usize,
    coronal: usize,
    sagittal: usize,
    plane: MprPlane,
    mode: ViewMode,
}

impl DicomViewApp {
    fn new(paths: Option<Vec<PathBuf>>) -> Self {
        // Paths passed programmatically (CLI/`diface --view`/tests) are not
        // recorded in the recent-folders history.
        let pending_load = paths.map(|p| (p, false));
        Self {
            pending_load,
            images: Vec::new(),
            series_groups: Vec::new(),
            viewports: vec![ViewportState::default()],
            active_viewport: 0,
            viewport_textures: vec![None],
            viewport_image_rects: Vec::new(),
            mpr_volumes: Vec::new(),
            mpr_errors: Vec::new(),
            recent_folders: Self::load_recent_folders(),
            texture: None,
            thumbnail_textures: HashMap::new(),
            zoom: 1.0,
            rotation_degrees: 0.0,
            rotation_drag_last_pos: None,
            wheel_slice_accumulator: 0.0,
            wheel_owner: None,
            pan: Vec2::ZERO,
            error: None,
            show_metadata: true,
            files_hovered: false,
            loading: None,
            deface_message: None,
            deface_params: GeometricParams::default(),
            deface_backend: DefaceBackendSel::Geometric,
            deface_atlas: diface_rs::Atlas::synthetic([128, 160, 140], 1.0),
            deface_panel_open: false,
            deface_show_segmentation: false,
            deface_preview_on: true,
            deface_seg_region: diface_rs::MaskRegion::Face,
            deface_deflesh_posterior_mm: 0.0,
            deface_preview: None,
            wl_editor_open: Vec::new(),
            last_navs: Vec::new(),
        }
    }

    fn load_files(&mut self, paths: Vec<PathBuf>, record_recent: bool, ctx: &egui::Context) {
        // Keep original input so we can record the originating folder for recent list
        let orig_paths = paths.clone();
        let paths = collect_dicom_files_recursively(paths);
        self.error = None;
        self.images.clear();
        self.series_groups.clear();
        // reset to a single default viewport
        self.viewports = vec![ViewportState::default()];
        self.active_viewport = 0;
        self.viewport_textures = vec![None];
        self.viewport_image_rects.clear();
        self.mpr_volumes.clear();
        self.mpr_errors.clear();
        if let Some(vp) = self.viewports.get_mut(self.active_viewport) {
            vp.pan = Vec2::ZERO;
            vp.zoom = 1.0;
            vp.rotation_degrees = 0.0;
            vp.rotation_drag_last_pos = None;
        }
        self.wheel_slice_accumulator = 0.0;
        self.wheel_owner = None;

        if paths.is_empty() {
            self.error = Some("No DICOM files found (searched recursively, including extensionless files)".to_string());
            self.loading = None;
            return;
        }

        // Record a recent folder entry: prefer a chosen folder, otherwise use the parent.
        // Only user-initiated opens are remembered (not CLI/`diface --view`/tests).
        if record_recent {
            if let Some(folder_candidate) = orig_paths.get(0) {
                let folder = if folder_candidate.is_dir() {
                    folder_candidate.clone()
                } else {
                    folder_candidate.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| folder_candidate.clone())
                };
                self.push_recent_folder(folder);
            }
        }

        let total = paths.len();
        let (tx, rx) = mpsc::channel::<LoadMsg>();
        let ctx_clone = ctx.clone();

        // Decode files in parallel using rayon; send results back to UI as they complete.
        std::thread::spawn(move || {
            paths.into_par_iter().for_each(|path| {
                let msg = match decode_single_file(&path) {
                    Ok(image) => LoadMsg::Image(image),
                    Err(e) => LoadMsg::Error(e),
                };
                let _ = tx.send(msg);
                ctx_clone.request_repaint();
            });
        });

        self.loading = Some(LoadingState {
            rx,
            total,
            received: 0,
            current_filename: String::new(),
        });
    }

    /// Drain the background loading channel and, once complete, rebuild the
    /// series groups and textures. Shared by the frame loop and tests.
    fn poll_loading(&mut self, ctx: &egui::Context) {
        let mut loading_complete = false;
        if let Some(state) = &mut self.loading {
            loop {
                match state.rx.try_recv() {
                    Ok(LoadMsg::Image(image)) => {
                        state.received += 1;
                        state.current_filename = image.filename.clone();
                        if let Some((tw, th, ref rgba)) = image.thumbnail {
                            let color_image =
                                egui::ColorImage::from_rgba_unmultiplied([tw, th], rgba);
                            let key = image.filename.clone();
                            let tex = ctx.load_texture(
                                format!("thumb_{}", key),
                                color_image,
                                egui::TextureOptions::LINEAR,
                            );
                            self.thumbnail_textures.insert(key.clone(), tex);
                        }
                        self.images.push(image);
                    }
                    Ok(LoadMsg::Error(e)) => {
                        state.received += 1;
                        self.error = Some(e);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        loading_complete = true;
                        break;
                    }
                }
            }
            if let Some(state) = &self.loading {
                if state.received >= state.total {
                    loading_complete = true;
                }
            }
        }
        if loading_complete {
            self.loading = None;
            self.rebuild_series_groups();
            self.apply_default_window_for_current_series();
            self.update_current_slice_view(ctx);
        }
    }

    fn vp(&self) -> &ViewportState {
        &self.viewports[self.active_viewport]
    }

    fn vp_mut(&mut self) -> &mut ViewportState {
        &mut self.viewports[self.active_viewport]
    }

    fn texture_for_active(&self) -> Option<egui::TextureHandle> {
        self.viewport_textures
            .get(self.active_viewport)
            .and_then(|o| o.clone())
    }

    fn active_series_len(&self) -> usize {
        self.series_groups
            .get(self.vp().current_series)
            .map(|g| g.image_indices.len())
            .unwrap_or(0)
    }

    fn active_image(&self) -> Option<&LoadedImage> {
        let group = self.series_groups.get(self.vp().current_series)?;
        let global_idx = *group.image_indices.get(self.vp().current_stack_slice)?;
        self.images.get(global_idx)
    }

    fn metadata_image(&self) -> Option<&LoadedImage> {
        self.active_image().or_else(|| {
            let group = self.series_groups.get(self.vp().current_series)?;
            let first = *group.image_indices.first()?;
            self.images.get(first)
        })
    }

    fn mpr_available(&self) -> bool {
        self.active_mpr_volume().is_some()
    }

    /// The MPR volume for the active viewport's own series (resolved via its
    /// base series, so a defaced series reuses its original's volume).
    fn active_mpr_volume(&self) -> Option<&MprVolume> {
        let idx = self.mpr_series_for(self.active_viewport)?;
        self.mpr_volumes.get(idx).and_then(|o| o.as_ref())
    }

    /// The per-series MPR build error for the active viewport.
    fn active_mpr_error(&self) -> Option<&str> {
        let idx = self.mpr_series_for(self.active_viewport)?;
        self.mpr_errors.get(idx).and_then(|o| o.as_deref())
    }

    /// The series whose MPR volume a viewport should use.
    ///
    /// Because volumes are built per series, this is always the viewport's own
    /// series. Falling back to the base series here would make a defaced
    /// viewport's MPR show the original (and vice versa).
    fn mpr_series_for(&self, viewport: usize) -> Option<usize> {
        let series = self.viewports.get(viewport)?.current_series;
        self.series_groups.get(series)?;
        Some(series)
    }

    fn current_mpr_slice(&self) -> usize {
        match self.vp().mpr_plane {
            MprPlane::Axial => self.vp().current_axial_slice,
            MprPlane::Coronal => self.vp().current_coronal_slice,
            MprPlane::Sagittal => self.vp().current_sagittal_slice,
        }
    }

    fn set_current_mpr_slice(&mut self, index: usize) {
        match self.vp().mpr_plane {
            MprPlane::Axial => self.vp_mut().current_axial_slice = index,
            MprPlane::Coronal => self.vp_mut().current_coronal_slice = index,
            MprPlane::Sagittal => self.vp_mut().current_sagittal_slice = index,
        }
    }

    /// Feed wheel input to the active viewport's slice navigation.
    ///
    /// `smooth_scroll_delta` arrives as a decaying tail after each physical
    /// notch, so stepping on every non-zero delta would skip several slices per
    /// notch. Instead the delta is accumulated and at most one slice is
    /// advanced per call once `WHEEL_SLICE_THRESHOLD` is reached, with the
    /// accumulator reset afterwards. `owner` identifies the viewport being
    /// scrolled so a hover change discards partial movement.
    ///
    /// Returns `true` when the slice changed.
    fn wheel_step_slices(
        &mut self,
        owner: usize,
        delta: f32,
        ctx: &egui::Context,
    ) -> bool {
        if self.current_view_slice_len() <= 1 {
            return false;
        }
        if self.wheel_owner != Some(owner) {
            self.wheel_owner = Some(owner);
            self.wheel_slice_accumulator = 0.0;
        }
        self.wheel_slice_accumulator += delta;
        if self.wheel_slice_accumulator.abs() < WHEEL_SLICE_THRESHOLD {
            return false;
        }

        // Positive wheel delta (scroll up) moves to the previous slice.
        let forward = self.wheel_slice_accumulator > 0.0;
        self.wheel_slice_accumulator = 0.0;

        let current = match self.vp().view_mode {
            ViewMode::Stack => self.vp().current_stack_slice,
            ViewMode::Mpr => self.current_mpr_slice(),
        };
        let len = self.current_view_slice_len();
        let new_index = if forward {
            current.saturating_sub(1)
        } else {
            (current + 1).min(len.saturating_sub(1))
        };
        if new_index == current {
            return false;
        }

        match self.vp().view_mode {
            ViewMode::Stack => self.vp_mut().current_stack_slice = new_index,
            ViewMode::Mpr => self.set_current_mpr_slice(new_index),
        }
        self.vp_mut().wl_dirty = true;
        self.update_current_slice_view(ctx);
        true
    }

    /// Change the active viewport's zoom while keeping `anchor` (a screen
    /// position, e.g. the mouse cursor) over the same image point. `center` is
    /// the centre of the viewport rect the image is drawn in. Because the image
    /// is rotated about `center`, adjusting the pan this way keeps the anchored
    /// point fixed for any rotation angle.
    fn zoom_towards(&mut self, new_zoom: f32, anchor: egui::Pos2, center: egui::Pos2) {
        let new_zoom = new_zoom.clamp(0.05, 20.0);
        let old_zoom = self.vp().zoom;
        if (new_zoom - old_zoom).abs() < f32::EPSILON {
            return;
        }
        let k = new_zoom / old_zoom;
        let pan = self.vp().pan;
        let pan_new = egui::vec2(
            (1.0 - k) * (anchor.x - center.x) + k * pan.x,
            (1.0 - k) * (anchor.y - center.y) + k * pan.y,
        );
        let vp = self.vp_mut();
        vp.zoom = new_zoom;
        vp.pan = pan_new;
    }

    // Return plane geometry for a viewport: origin (patient), normal, column_dir, row_dir, col_spacing, row_spacing, width, height
    fn viewport_plane(&self, idx: usize) -> Option<([f32; 3], [f32; 3], [f32; 3], [f32; 3], f32, f32, usize, usize)> {
        let vp = self.viewports.get(idx)?;
        if vp.view_mode == ViewMode::Mpr {
            let vol = self.mpr_volumes.get(self.mpr_series_for(idx)?)?.as_ref()?;
            // MPR shows axis-aligned patient planes; describe the displayed
            // plane so cross-references are correct.
            let (u_axis, v_axis, w_axis) = vp.mpr_plane.patient_axes();
            let u_sign = vp.mpr_plane.u_sign();
            let v_sign = vp.mpr_plane.v_sign();
            let u_dir = scale(u_axis.unit(), u_sign);
            let v_dir = scale(v_axis.unit(), v_sign);
            let w_dir = w_axis.unit();
            let center = vol.patient_min;
            let w_coord = vol.axis_coordinate(w_axis, self.mpr_slice_for(idx));
            let mut origin = center;
            origin[w_axis.index()] = w_coord;
            return Some((
                origin,
                w_dir,
                normalize(u_dir)?,
                normalize(v_dir)?,
                vol.axis_spacing(u_axis),
                vol.axis_spacing(v_axis),
                vol.axis_samples(u_axis),
                vol.axis_samples(v_axis),
            ));
        } else {
            let group = self.series_groups.get(vp.current_series)?;
            let idx_in_group = vp.current_stack_slice.min(group.image_indices.len().saturating_sub(1));
            let img = self.images.get(*group.image_indices.get(idx_in_group)?)?;
            let pos = img.image_position_patient?;
            let orient = img.image_orientation_patient?;
            let col_dir = [orient[0], orient[1], orient[2]];
            let row_dir = [orient[3], orient[4], orient[5]];
            let normal = normalize(cross(col_dir, row_dir))?;
            let pixel_spacing = img.pixel_spacing.unwrap_or([1.0, 1.0]);
            let (w, h) = img.raw_image.dimensions();
            return Some((pos, normal, col_dir, row_dir, pixel_spacing[1].abs().max(0.001), pixel_spacing[0].abs().max(0.001), w, h));
        }
    }

    // Intersect two planes (n1·x = d1, n2·x = d2). Returns (point_on_line, direction)
    fn intersect_planes(n1: [f32; 3], p1: [f32; 3], n2: [f32; 3], p2: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
        let d1 = dot(n1, p1);
        let d2 = dot(n2, p2);
        let dir = cross(n1, n2);
        let denom = dot(dir, dir);
        if denom.abs() < 1e-6 {
            return None;
        }
        // point = ( (d1 * (n2 x dir)) + (d2 * (dir x n1)) ) / denom
        let term1 = scale(cross(n2, dir), d1);
        let term2 = scale(cross(dir, n1), d2);
        let point = scale(add(term1, term2), 1.0 / denom);
        Some((point, dir))
    }

    // Project a patient-space point into image pixel coordinates for a viewport plane
    fn project_point_to_pixel(
        &self,
        origin: [f32; 3],
        col_dir: [f32; 3],
        row_dir: [f32; 3],
        col_spacing: f32,
        row_spacing: f32,
        p: [f32; 3],
    ) -> (f32, f32) {
        let v = sub(p, origin);
        let u = dot(v, col_dir) / col_spacing; // column index
        let vcoord = dot(v, row_dir) / row_spacing; // row index
        (u, vcoord)
    }

    // Convert image pixel coords to screen pos inside a cell (handles pan/zoom/rotation similar to texture drawing)
    fn pixel_to_screen(
        &self,
        u: f32,
        v: f32,
        img_w: f32,
        img_h: f32,
        cell_rect: egui::Rect,
        vp: &ViewportState,
    ) -> egui::Pos2 {
        let physical_size = vp.displayed_physical_size.unwrap_or_else(|| egui::vec2(img_w, img_h));
        let display = if vp.view_mode == ViewMode::Mpr && !vp.scale_by_physical {
            let fit = (cell_rect.width() / img_w).min(cell_rect.height() / img_h);
            egui::vec2(img_w * fit * vp.zoom, img_h * fit * vp.zoom)
        } else {
            let fit = (cell_rect.width() / physical_size.x).min(cell_rect.height() / physical_size.y);
            egui::vec2(physical_size.x * fit * vp.zoom, physical_size.y * fit * vp.zoom)
        };
        let center = cell_rect.center() + vp.pan;
        // pixel coords origin top-left; convert to centered coordinates
        let dx = (u - img_w * 0.5) * (display.x / img_w);
        let dy = (v - img_h * 0.5) * (display.y / img_h);
        let rotated = rotate_vec2(egui::vec2(dx, dy), vp.rotation_degrees.to_radians());
        center + rotated
    }

    // --- Recent folders persistence ---------------------------------
    fn recent_config_path() -> Option<PathBuf> {
        ProjectDirs::from("io", "penra", "diviz-rs").map(|pd| pd.config_dir().join("recent_folders.json"))
    }

    fn load_recent_folders() -> Vec<PathBuf> {
        if let Some(path) = Self::recent_config_path() {
            if path.exists() {
                if let Ok(mut f) = File::open(&path) {
                    let mut s = String::new();
                    if f.read_to_string(&mut s).is_ok() {
                        if let Ok(list) = serde_json::from_str::<Vec<String>>(&s) {
                            return list.into_iter().map(PathBuf::from).collect();
                        }
                    }
                }
            }
        }
        Vec::new()
    }

    fn save_recent_folders(&self) {
        if let Some(path) = Self::recent_config_path() {
            if let Some(dir) = path.parent() {
                let _ = create_dir_all(dir);
            }
            let strings: Vec<String> = self.recent_folders.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            if let Ok(mut f) = File::create(&path) {
                let _ = serde_json::to_writer_pretty(&mut f, &strings);
                let _ = f.flush();
            }
        }
    }

    fn push_recent_folder(&mut self, folder: PathBuf) {
        // Remove duplicates and insert at front
        self.recent_folders.retain(|p| p != &folder);
        self.recent_folders.insert(0, folder);
        // cap to 10 entries
        if self.recent_folders.len() > 10 {
            self.recent_folders.truncate(10);
        }
        self.save_recent_folders();
    }

    fn current_view_slice_len(&self) -> usize {
        match self.vp().view_mode {
            ViewMode::Stack => self.active_series_len(),
            ViewMode::Mpr => self
                .active_mpr_volume()
                .map(|volume| volume.plane_len(self.vp().mpr_plane))
                .unwrap_or(0),
        }
    }

    fn apply_default_window_for_current_series(&mut self) {
        match self.vp().view_mode {
            ViewMode::Stack => {
                if let Some(img) = self.active_image() {
                    if let Some((wc, ww)) = img.default_wc_ww {
                        self.vp_mut().window_center = wc;
                        self.vp_mut().window_width = ww;
                    }
                }
            }
            ViewMode::Mpr => {
                if let Some(volume) = self.active_mpr_volume() {
                    let (wc, ww) = volume.default_wc_ww;
                    self.vp_mut().window_center = wc;
                    self.vp_mut().window_width = ww;
                }
            }
        }
    }

    /// Default (DICOM or data-range) window/level for the active viewport.
    fn active_default_wc_ww(&self) -> Option<(f32, f32)> {
        match self.vp().view_mode {
            ViewMode::Stack => self.active_image().and_then(|img| img.default_wc_ww),
            ViewMode::Mpr => self.active_mpr_volume().map(|volume| volume.default_wc_ww),
        }
    }

    /// DICOM Modality of the active series, upper-cased (e.g. "CT", "MR").
    fn active_modality(&self) -> Option<String> {
        self.active_image().and_then(|img| {
            img.metadata
                .iter()
                .find(|(label, _)| label == "Modality")
                .map(|(_, value)| value.trim().to_uppercase())
        })
    }

    /// Whether the active series can be defaced (grayscale 16-bit volume).
    fn active_series_is_grayscale16(&self) -> bool {
        self.series_groups
            .get(self.vp().current_series)
            .and_then(|g| g.image_indices.first())
            .and_then(|i| self.images.get(*i))
            .map(|img| img.raw_image.is_grayscale16())
            .unwrap_or(false)
    }

    /// Deface the active stack in memory and show the result next to it.
    ///
    /// A copy of each slice is created with facial voxels blanked, grouped as a
    /// new series, and opened in a second viewport for direct comparison.
    /// The base (non-defaced) series group for the active viewport, resolving a
    /// defaced copy back to its original.
    fn deface_base_group(&self) -> Option<SeriesGroup> {
        let active_group = self.series_groups.get(self.vp().current_series)?.clone();
        let base_uid = active_group
            .uid
            .strip_suffix("-DEFACED")
            .unwrap_or(&active_group.uid)
            .to_string();
        Some(
            self.series_groups
                .iter()
                .find(|g| g.uid == base_uid)
                .cloned()
                .unwrap_or(active_group),
        )
    }

    /// Build the deface geometry and removal mask for a series group with the
    /// current settings (geometric algorithm or atlas template).
    fn compute_deface_mask(
        &self,
        group: &SeriesGroup,
    ) -> Result<(DefaceGeometry, diface_rs::volume::Mask), String> {
        let geom = build_deface_volume(&self.images, &group.image_indices)?;
        let mask = match self.deface_backend {
            DefaceBackendSel::Atlas => diface_rs::AtlasBackend::new(
                self.deface_atlas.clone(),
                diface_rs::AtlasParams::default(),
            )
            .compute_mask(&geom.volume)?,
            DefaceBackendSel::Segmentation => {
                let p = &self.deface_params;
                // Map the geometric alignment controls onto the segmentation
                // backend so the same sliders/presets apply:
                //  - Depth (`anterior_offset_mm`) reduces the safety margin in
                //    front of the brain (positive == removes more face).
                //  - Preset/preserve fraction widens the margin (more brain-safe).
                let margin = (12.0 - p.anterior_offset_mm
                    - (p.preserve_fraction - 0.82) * 40.0)
                    .clamp(0.0, 40.0);
                let params = diface_rs::SegBackendParams {
                    region: self.deface_seg_region,
                    brain_margin_mm: margin,
                    deflesh_posterior_mm: self.deface_deflesh_posterior_mm,
                    seg: diface_rs::SegParams::default(),
                    ..diface_rs::SegBackendParams::default()
                };
                diface_rs::SegmentationBackend::new(params).compute_mask(&geom.volume)?
            }
            DefaceBackendSel::Geometric => {
                GeometricBackend::new(self.deface_params.clone()).compute_mask(&geom.volume)?
            }
        };
        Ok((geom, mask))
    }

    /// Toggle the live removal-mask preview for the active series.
    fn toggle_deface_preview(&mut self, ctx: &egui::Context) {
        self.deface_preview_on = !self.deface_preview_on;
        if self.deface_preview_on {
            self.refresh_deface_preview(ctx);
        } else {
            self.deface_preview = None;
            self.redraw_all_viewports(ctx);
            self.deface_message = Some("Mask preview off".to_string());
        }
    }

    /// Recompute the removal-mask preview for the active base series.
    fn refresh_deface_preview(&mut self, ctx: &egui::Context) {
        let Some(group) = self.deface_base_group() else {
            return;
        };
        match self.compute_deface_mask(&group) {
            Ok((geom, mask)) => {
                let nx = geom.width;
                let ny = geom.height;
                let nz = geom.order.len();
                let n = nx * ny;

                // Full 3D mask in the volume grid (shared by MPR and Stack).
                let mut volume_flags = vec![false; n * nz];
                // Per-image in-plane flags for Stack viewports.
                let mut per_image: HashMap<usize, Vec<bool>> = HashMap::new();
                for (z, &image_idx) in geom.order.iter().enumerate() {
                    let mut flags = vec![false; n];
                    for y in 0..ny {
                        for x in 0..nx {
                            if geom.is_removed_native(x, y, z, &mask) {
                                flags[y * nx + x] = true;
                                volume_flags[(z * ny + y) * nx + x] = true;
                            }
                        }
                    }
                    per_image.insert(image_idx, flags);
                }
                let removed = mask.count_removed();

                // Optional segmentation-area labels for the overlay.
                let (volume_labels, per_image_labels) = if self.deface_show_segmentation {
                    match diface_rs::segment(&geom.volume, &diface_rs::SegParams::default()) {
                        Ok(seg) => {
                            let mut vl = vec![0u8; n * nz];
                            let mut pil: HashMap<usize, Vec<u8>> = HashMap::new();
                            for (z, &image_idx) in geom.order.iter().enumerate() {
                                let mut row = vec![0u8; n];
                                for y in 0..ny {
                                    for x in 0..nx {
                                        let cx = (x / seg.stride[0]).min(seg.dims[0] - 1);
                                        let cy = (y / seg.stride[1]).min(seg.dims[1] - 1);
                                        let cz = (z / seg.stride[2]).min(seg.dims[2] - 1);
                                        let label = if seg.brain_at(cx, cy, cz) {
                                            2u8
                                        } else if seg.cavity_at(cx, cy, cz) {
                                            4u8
                                        } else if seg.vault_at(cx, cy, cz) {
                                            3u8
                                        } else if seg.head_at(cx, cy, cz) {
                                            1
                                        } else {
                                            0
                                        };
                                        row[y * nx + x] = label;
                                        vl[(z * ny + y) * nx + x] = label;
                                    }
                                }
                                pil.insert(image_idx, row);
                            }
                            (Some(vl), Some(pil))
                        }
                        Err(_) => (None, None),
                    }
                } else {
                    (None, None)
                };

                self.deface_preview = Some(DefacePreview {
                    uid: group.uid.clone(),
                    volume_flags,
                    dims: [nx, ny, nz],
                    per_image,
                    volume_labels,
                    per_image_labels,
                });
                self.deface_message = Some(format!(
                    "Previewing {removed} voxels to remove (red overlay)"
                ));
                self.redraw_all_viewports(ctx);
            }
            Err(e) => {
                self.deface_preview = None;
                self.deface_message = Some(e);
            }
        }
    }

    /// Rebuild the texture of every viewport.
    fn redraw_all_viewports(&mut self, ctx: &egui::Context) {
        let saved = self.active_viewport;
        for i in 0..self.viewports.len() {
            self.update_slice_view_for(i, ctx);
        }
        self.active_viewport = saved;
    }

    fn deface_active_series(&mut self, ctx: &egui::Context) {
        self.deface_message = None;

        let Some(group) = self.deface_base_group() else {
            self.deface_message = Some("No active series to deface".to_string());
            return;
        };

        let (geom, mask) = match self.compute_deface_mask(&group) {
            Ok(v) => v,
            Err(e) => {
                self.deface_message = Some(e);
                return;
            }
        };
        let order = geom.order.clone();

        let fill = geom.volume.raw_min_max().0;
        let removed = mask.count_removed();

        let defaced_uid = format!("{}-DEFACED", group.uid);
        let mut new_images: Vec<LoadedImage> = Vec::with_capacity(order.len());
        for (z, &idx) in order.iter().enumerate() {
            let Some(mut img) = self.images.get(idx).cloned() else {
                continue;
            };
            let (width, height) = img.raw_image.dimensions();
            if let RawImage::Gray16(g) = &mut img.raw_image {
                for y in 0..height {
                    let row = y * width;
                    for x in 0..width {
                        if geom.is_removed_native(x, y, z, &mask) && row + x < g.data.len() {
                            g.data[row + x] = fill;
                        }
                    }
                }
            }
            img.series_uid = defaced_uid.clone();
            img.series_label = format!("{} [defaced]", img.series_label);
            img.filename = format!("defaced_{}", img.filename);
            img.thumbnail = img
                .default_wc_ww
                .map(|(wc, ww)| make_thumbnail(&img.raw_image, 64, wc, ww));
            new_images.push(img);
        }
        if new_images.is_empty() {
            self.deface_message = Some("Nothing to deface".to_string());
            return;
        }
        let defaced_count = new_images.len();

        // Register thumbnails for the series list.
        for img in &new_images {
            if let Some((tw, th, rgba)) = img.thumbnail.as_ref() {
                let color = egui::ColorImage::from_rgba_unmultiplied([*tw, *th], rgba);
                let tex = ctx.load_texture(
                    format!("thumb_{}", img.filename),
                    color,
                    egui::TextureOptions::LINEAR,
                );
                self.thumbnail_textures.insert(img.filename.clone(), tex);
            }
        }

        // Drop any previous defaced copy so repeated clicks refresh it rather
        // than merging duplicate slices into one series.
        let had_previous = self.series_groups.iter().any(|g| g.uid == defaced_uid);
        if had_previous {
            self.images.retain(|img| img.series_uid != defaced_uid);
        }

        self.images.extend(new_images);
        self.rebuild_series_groups();

        let orig_uid = group.uid.clone();
        let orig_idx = self.series_groups.iter().position(|g| g.uid == orig_uid);
        let def_idx = self.series_groups.iter().position(|g| g.uid == defaced_uid);
        let (Some(orig_idx), Some(def_idx)) = (orig_idx, def_idx) else {
            self.deface_message =
                Some("Defaced series was created but could not be located".to_string());
            return;
        };

        // Ensure two panes and show original next to defaced.
        while self.viewports.len() < 2 {
            self.viewports.push(ViewportState::default());
            self.viewport_textures.push(None);
        }
        let (show_a, show_b) = if self.viewports.len() == 2 {
            (0usize, 1usize)
        } else {
            let a = self.active_viewport.min(self.viewports.len() - 1);
            (a, (a + 1) % self.viewports.len())
        };

        for (slot, series_idx) in [(show_a, orig_idx), (show_b, def_idx)] {
            let vp = &mut self.viewports[slot];
            vp.current_series = series_idx;
            vp.view_mode = ViewMode::Stack;
            vp.current_stack_slice = 0;
            vp.current_axial_slice = 0;
            vp.current_coronal_slice = 0;
            vp.current_sagittal_slice = 0;
            vp.wl_dirty = true;
        }
        // Share the viewing geometry so the two panes line up.
        self.viewports[show_b].zoom = self.viewports[show_a].zoom;
        self.viewports[show_b].pan = self.viewports[show_a].pan;
        self.viewports[show_b].rotation_degrees = self.viewports[show_a].rotation_degrees;

        // Set per-pane window/level from each series, then build both textures.
        for slot in [show_a, show_b] {
            self.active_viewport = slot;
            self.apply_default_window_for_current_series();
        }
        for slot in [show_a, show_b] {
            self.active_viewport = slot;
            self.update_current_slice_view(ctx);
        }
        self.active_viewport = show_b;
        self.refresh_mpr_volumes();

        // Lock the two panes together so scrolling one follows in the other.
        let group = self.next_sync_group();
        self.viewports[show_a].sync_group = Some(group);
        self.viewports[show_b].sync_group = Some(group);
        self.sync_from(show_a, ctx);
        self.last_navs = self.all_nav_snapshots();

        // The defaced copy is now materialised. If the preview is enabled, keep
        // showing it on the base series (re-enabled after applying).
        self.deface_preview = None;
        if self.deface_preview_on {
            // Show the preview on the *original* viewport, not the defaced copy.
            self.active_viewport = show_a;
            self.refresh_deface_preview(ctx);
            self.active_viewport = show_b;
        }

        self.deface_message = Some(format!(
            "Defaced {defaced_count} slices ({removed} voxels blanked) - original vs defaced shown side by side (synced)"
        ));
    }

    /// Manual defacing alignment/depth/extent controls.
    ///
    /// Edits `self.deface_params`; pressing "Re-apply" rebuilds the defaced
    /// copy from the base series with the current settings.
    fn deface_alignment_ui(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                // Backend row: geometric, atlas template, or in-house segmentation.
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Backend");
                    let mut sel = self.deface_backend;
                    ui.selectable_value(&mut sel, DefaceBackendSel::Geometric, "Geometric");
                    ui.selectable_value(&mut sel, DefaceBackendSel::Atlas, "Atlas")
                        .on_hover_text("Register a brain template and cut in front of it");
                    ui.selectable_value(&mut sel, DefaceBackendSel::Segmentation, "Segmentation")
                        .on_hover_text("In-house head/brain segmentation; cut in front of the brain");
                    if sel != self.deface_backend {
                        self.deface_backend = sel;
                        if self.deface_preview.is_some() {
                            self.refresh_deface_preview(ui.ctx());
                        }
                    }
                    if self.deface_backend == DefaceBackendSel::Segmentation {
                        ui.separator();
                        ui.label("Region");
                        let mut region = self.deface_seg_region;
                        ui.selectable_value(&mut region, diface_rs::MaskRegion::Face, "Face")
                            .on_hover_text("Flat anterior cut in front of the brain");
                        ui.selectable_value(
                            &mut region,
                            diface_rs::MaskRegion::ExternalSoftTissue,
                            "Deflesh",
                        )
                        .on_hover_text(
                            "Remove external soft tissue and bone (scalp/face/table) outside \
                             the brain",
                        );
                        if region != self.deface_seg_region {
                            self.deface_seg_region = region;
                            if self.deface_preview.is_some() {
                                self.refresh_deface_preview(ui.ctx());
                            }
                        }
                        if self.deface_seg_region == diface_rs::MaskRegion::ExternalSoftTissue {
                            ui.separator();
                            ui.label("Keep back-of-head after");
                            let mut back = self.deface_deflesh_posterior_mm;
                            let slider = ui.add(
                                egui::Slider::new(&mut back, 0.0..=80.0)
                                    .suffix(" mm")
                                    .clamping(egui::SliderClamping::Always),
                            );
                            if slider
                                .on_hover_text(
                                    "Keep external tissue posterior of brain centre + this \
                                     distance (keeps the neck/back of the head)",
                                )
                                .changed()
                            {
                                self.deface_deflesh_posterior_mm = back;
                                if self.deface_preview.is_some() {
                                    self.refresh_deface_preview(ui.ctx());
                                }
                            }
                        }
                    }
                    if ui
                        .button("Load atlas…")
                        .on_hover_text("Load a .dfatlas brain mask")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("diface atlas", &["dfatlas", "bin"])
                            .pick_file()
                        {
                            match std::fs::read(&path)
                                .map_err(|e| e.to_string())
                                .and_then(|b| diface_rs::Atlas::from_bytes(&b))
                            {
                                Ok(a) => {
                                    self.deface_atlas = a;
                                    self.deface_backend = DefaceBackendSel::Atlas;
                                    self.deface_message =
                                        Some(format!("Loaded atlas {}", path.display()));
                                    if self.deface_preview.is_some() {
                                        self.refresh_deface_preview(ui.ctx());
                                    }
                                }
                                Err(e) => {
                                    self.deface_message = Some(format!("Atlas load failed: {e}"))
                                }
                            }
                        }
                    }
                    if self.deface_backend == DefaceBackendSel::Atlas {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}×{}×{}",
                                self.deface_atlas.nx(),
                                self.deface_atlas.ny(),
                                self.deface_atlas.nz()
                            ))
                            .small()
                            .weak(),
                        );
                    }
                });

                // Algorithm row: pick the masking strategy (geometric backend).
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Algorithm");
                    let enabled = self.deface_backend == DefaceBackendSel::Geometric;
                    let mut chosen = self.deface_params.algorithm;
                    ui.add_enabled_ui(enabled, |ui| {
                        egui::ComboBox::from_id_salt("deface_algorithm")
                            .selected_text(chosen.name())
                            .show_ui(ui, |ui| {
                                for algo in DefaceAlgorithm::all() {
                                    ui.selectable_value(&mut chosen, algo, algo.name())
                                        .on_hover_text(algo.description());
                                }
                            });
                    });
                    if chosen != self.deface_params.algorithm {
                        self.deface_params.algorithm = chosen;
                        if self.deface_preview.is_some() {
                            self.refresh_deface_preview(ui.ctx());
                        }
                    }
                    let note = if self.deface_backend == DefaceBackendSel::Geometric {
                        self.deface_params.algorithm.description().to_string()
                    } else {
                        self.deface_backend.name().to_string()
                    };
                    ui.label(egui::RichText::new(note).small().weak());
                });

                // Preset row: quick, brain-safety-oriented choices.
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Preset");
                    let matching = self.deface_params.matching_preset();
                    let mut apply: Option<DefacePreset> = None;
                    for preset in DefacePreset::all() {
                        let selected = matching == Some(preset);
                        if ui
                            .selectable_label(selected, preset.name())
                            .on_hover_text(preset.description())
                            .clicked()
                        {
                            apply = Some(preset);
                        }
                    }
                    if let Some(preset) = apply {
                        // Preserve manual orientation (yaw/axis override).
                        self.deface_params.apply_preset(preset);
                        if self.deface_preview.is_some() {
                            self.refresh_deface_preview(ui.ctx());
                        }
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    ui.strong("Deface alignment");
                    ui.label(
                        egui::RichText::new("(applies to the defaced copy of the active series)")
                            .small()
                            .weak(),
                    );

                    // Yaw about the superior axis.
                    ui.label("Yaw°");
                    let yaw_resp = ui.add(
                        egui::DragValue::new(&mut self.deface_params.yaw_deg)
                            .speed(1.0)
                            .range(-180.0..=180.0),
                    );

                    // Anterior/posterior depth of the cut.
                    ui.label("Depth mm");
                    let depth_resp = ui.add(
                        egui::DragValue::new(&mut self.deface_params.anterior_offset_mm)
                            .speed(1.0)
                            .range(-100.0..=100.0),
                    );

                    // Preserve-ellipsoid size along each anatomical axis.
                    ui.label("Extent A/L/S");
                    let mut extent_changed = false;
                    for axis in 0..3 {
                        let r = ui.add(
                            egui::DragValue::new(&mut self.deface_params.extent_scale[axis])
                                .speed(0.01)
                                .range(0.1..=3.0),
                        );
                        extent_changed |= r.changed();
                    }

                    // Coarse preserve fraction (overall safety margin).
                    ui.label("Preserve");
                    let preserve = ui.add(
                        egui::DragValue::new(&mut self.deface_params.preserve_fraction)
                            .speed(0.01)
                            .range(0.3..=1.2),
                    );

                    let changed =
                        yaw_resp.changed() || depth_resp.changed() || preserve.changed() || extent_changed;

                    let previewing = self.deface_preview_on;
                    let toggle = ui
                        .selectable_label(previewing, "👁 Preview")
                        .on_hover_text("Show the voxels that would be removed (red overlay)")
                        .clicked();
                    if toggle {
                        self.toggle_deface_preview(ui.ctx());
                    } else if changed && previewing {
                        // Live-update the overlay as parameters change.
                        self.refresh_deface_preview(ui.ctx());
                    }

                    if ui
                        .selectable_label(self.deface_show_segmentation, "🗺 Areas")
                        .on_hover_text(
                            "Show the segmented areas in the overlay: head (blue), brain \
                             (green), vault (orange), cavity (cyan)",
                        )
                        .clicked()
                    {
                        self.deface_show_segmentation = !self.deface_show_segmentation;
                        // (Re)build the preview so the areas are drawn.
                        self.refresh_deface_preview(ui.ctx());
                    }

                    if ui.button("↺ Auto").on_hover_text("Reset defacing settings to automatic").clicked() {
                        self.deface_params = GeometricParams::default();
                        if self.deface_preview.is_some() {
                            self.refresh_deface_preview(ui.ctx());
                        }
                    }
                    if ui
                        .button("🧭 Reset view")
                        .on_hover_text(
                            "Reset the active viewport's zoom, pan, rotation and window/level, and \
                             clear the preview overlay",
                        )
                        .clicked()
                    {
                        let vp = self.vp_mut();
                        vp.zoom = 1.0;
                        vp.rotation_degrees = 0.0;
                        vp.rotation_drag_last_pos = None;
                        vp.pan = Vec2::ZERO;
                        self.apply_default_window_for_current_series();
                        self.vp_mut().wl_dirty = true;
                        // Close the live preview too, so the view is back to raw.
                        self.deface_preview = None;
                        self.redraw_all_viewports(ui.ctx());
                    }
                    if ui
                        .button("✓ Re-apply")
                        .on_hover_text("Rebuild the defaced copy with these settings")
                        .clicked()
                    {
                        self.deface_active_series(ui.ctx());
                    }
                });
            });
    }

    fn rebuild_series_groups(&mut self) {
        let mut grouped: HashMap<String, SeriesGroup> = HashMap::new();

        for (idx, image) in self.images.iter().enumerate() {
            let entry = grouped
                .entry(image.series_uid.clone())
                .or_insert_with(|| SeriesGroup {
                    uid: image.series_uid.clone(),
                    label: image.series_label.clone(),
                    image_indices: Vec::new(),
                });
            entry.image_indices.push(idx);
        }

        let mut groups: Vec<SeriesGroup> = grouped.into_values().collect();

        for group in &mut groups {
            group.image_indices.sort_by(|a, b| {
                let ia = &self.images[*a];
                let ib = &self.images[*b];
                ia.instance_number
                    .cmp(&ib.instance_number)
                    .then_with(|| ia.filename.cmp(&ib.filename))
            });
        }

        groups.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.uid.cmp(&b.uid)));
        // Prefer the original series in a group when sorted labels tie (e.g.
        // "X" before "X [defaced]").
        groups.sort_by(|a, b| {
            b.image_indices
                .len()
                .cmp(&a.image_indices.len())
                .then_with(|| a.label.cmp(&b.label))
        });
        self.series_groups = groups;
        // reset viewport selections to first series
        for vp in &mut self.viewports {
            vp.current_series = 0;
            vp.current_stack_slice = 0;
            vp.current_axial_slice = 0;
            vp.current_coronal_slice = 0;
            vp.current_sagittal_slice = 0;
            vp.displayed_physical_size = None;
        }
        self.refresh_mpr_volumes();
    }

    /// Rebuild the MPR volume for every series, so all series (original and
    /// defaced copies alike) can be viewed in MPR independently and their
    /// planes never mix.
    fn refresh_mpr_volumes(&mut self) {
        let count = self.series_groups.len();
        self.mpr_volumes = vec![None; count];
        self.mpr_errors = vec![None; count];

        for series in 0..count {
            let indices = self.series_groups[series].image_indices.clone();
            match MprVolume::from_images(&self.images, &indices) {
                Ok(volume) => self.mpr_volumes[series] = Some(volume),
                Err(err) => self.mpr_errors[series] = Some(err),
            }
        }

        // Keep every MPR viewport on a valid plane and fall back to Stack when
        // its series has no usable volume.
        for i in 0..self.viewports.len() {
            if self.viewports[i].view_mode != ViewMode::Mpr {
                continue;
            }
            let Some(series) = self.mpr_series_for(i) else {
                self.viewports[i].view_mode = ViewMode::Stack;
                continue;
            };
            let Some(volume) = self.mpr_volumes.get(series).and_then(|o| o.as_ref()) else {
                self.viewports[i].view_mode = ViewMode::Stack;
                continue;
            };
            let plane = self.viewports[i].mpr_plane;
            let max = volume.plane_len(plane).saturating_sub(1);
            let value = self.viewports[i].mpr_slice_value();
            let clamped = if value == 0 { max / 2 } else { value.min(max) };
            self.viewports[i].set_mpr_slice_value(clamped);
        }
    }

    /// Set the texture for a specific viewport.
    fn set_texture_for(&mut self, idx: usize, tex: Option<egui::TextureHandle>) {
        if self.viewport_textures.len() <= idx {
            self.viewport_textures.resize(idx + 1, None);
        }
        self.viewport_textures[idx] = tex;
    }

    fn series_len_for(&self, idx: usize) -> usize {
        self.viewports
            .get(idx)
            .and_then(|vp| self.series_groups.get(vp.current_series))
            .map(|g| g.image_indices.len())
            .unwrap_or(0)
    }

    fn image_for(&self, idx: usize) -> Option<&LoadedImage> {
        let vp = self.viewports.get(idx)?;
        let group = self.series_groups.get(vp.current_series)?;
        let global_idx = *group.image_indices.get(vp.current_stack_slice)?;
        self.images.get(global_idx)
    }

    fn mpr_slice_for(&self, idx: usize) -> usize {
        match self.viewports.get(idx) {
            Some(vp) => match vp.mpr_plane {
                MprPlane::Axial => vp.current_axial_slice,
                MprPlane::Coronal => vp.current_coronal_slice,
                MprPlane::Sagittal => vp.current_sagittal_slice,
            },
            None => 0,
        }
    }

    fn set_mpr_slice_for(&mut self, idx: usize, value: usize) {
        let Some(vp) = self.viewports.get_mut(idx) else {
            return;
        };
        match vp.mpr_plane {
            MprPlane::Axial => vp.current_axial_slice = value,
            MprPlane::Coronal => vp.current_coronal_slice = value,
            MprPlane::Sagittal => vp.current_sagittal_slice = value,
        }
    }

    /// Build the texture for a specific viewport (not necessarily the active one).
    fn update_slice_view_for(&mut self, idx: usize, ctx: &egui::Context) {
        if self.viewports.get(idx).is_none() {
            return;
        }
        self.set_texture_for(idx, None);
        if let Some(vp) = self.viewports.get_mut(idx) {
            vp.displayed_physical_size = None;
        }

        let mode = self.viewports[idx].view_mode;
        let (wc, ww) = {
            let vp = &self.viewports[idx];
            (vp.window_center, vp.window_width)
        };

        match mode {
            ViewMode::Stack => {
                if self.series_len_for(idx) == 0 {
                    if let Some(vp) = self.viewports.get_mut(idx) {
                        vp.wl_dirty = false;
                    }
                    return;
                }
                // Global image index shown by this viewport, and whether a
                // removal-mask preview applies to it.
                let global_idx = self.viewports.get(idx).and_then(|vp| {
                    self.series_groups
                        .get(vp.current_series)
                        .and_then(|g| g.image_indices.get(vp.current_stack_slice).copied())
                });
                let preview_flags = global_idx
                    .and_then(|gi| self.deface_preview.as_ref().map(|p| (gi, p)))
                    .and_then(|(gi, p)| p.per_image.get(&gi));

                let Some((width, height, mut rgba, physical_size)) = self.image_for(idx).map(|img| {
                    let (width, height) = img.raw_image.dimensions();
                    (width, height, img.raw_image.to_rgba(wc, ww), img.physical_size())
                }) else {
                    if let Some(vp) = self.viewports.get_mut(idx) {
                        vp.wl_dirty = false;
                    }
                    return;
                };
                if let Some(flags) = preview_flags {
                    tint_removed(&mut rgba, flags);
                }
                if let Some(labels) = self
                    .deface_preview
                    .as_ref()
                    .and_then(|p| p.per_image_labels.as_ref())
                    .and_then(|m| global_idx.and_then(|gi| m.get(&gi)))
                {
                    tint_segmentation(&mut rgba, labels);
                }
                let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                let tex = ctx.load_texture(
                    format!("dicom_image_vp_{idx}"),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                self.set_texture_for(idx, Some(tex));
                if let Some(vp) = self.viewports.get_mut(idx) {
                    vp.displayed_physical_size = Some(physical_size);
                    vp.wl_dirty = false;
                }
                let _ = self.viewport_display_rect(width, height, physical_size, mode);
            }
            ViewMode::Mpr => {
                let (width, height, rgba, physical_size) = {
                    let Some(series) = self.mpr_series_for(idx) else {
                        if let Some(vp) = self.viewports.get_mut(idx) {
                            vp.wl_dirty = false;
                        }
                        return;
                    };
                    let Some(volume) = self.mpr_volumes.get(series).and_then(|o| o.as_ref()) else {
                        if let Some(vp) = self.viewports.get_mut(idx) {
                            vp.wl_dirty = false;
                        }
                        return;
                    };
                    let plane = self.viewports[idx].mpr_plane;
                    let plane_index = self.mpr_slice_for(idx);
                    let pixels = volume.extract_plane(plane, plane_index);
                    let (width, height) = volume.plane_dimensions(plane);
                    let mut rgba = scalar_to_rgba(&pixels, wc, ww);
                    // Overlay the removal preview when this MPR shows the base
                    // series and the preview grid matches the MPR volume.
                    let series_uid = self
                        .series_groups
                        .get(series)
                        .map(|g| g.uid.clone())
                        .unwrap_or_default();
                    if let Some(preview) = self.deface_preview.as_ref() {
                        let volume_dims = [volume.width, volume.height, volume.depth];
                        if preview.uid == series_uid && preview.dims == volume_dims {
                            let flags = preview.plane_flags(plane, plane_index);
                            tint_removed(&mut rgba, &flags);
                            if let Some(labels) = preview.plane_labels(plane, plane_index) {
                                tint_segmentation(&mut rgba, &labels);
                            }
                        }
                    }
                    let physical_size = volume.physical_size(plane);
                    (width, height, rgba, physical_size)
                };
                let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                let tex = ctx.load_texture(
                    format!("dicom_image_vp_{idx}"),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                self.set_texture_for(idx, Some(tex));
                if let Some(vp) = self.viewports.get_mut(idx) {
                    vp.displayed_physical_size = Some(physical_size);
                    vp.wl_dirty = false;
                }
                let _ = self.viewport_display_rect(width, height, physical_size, mode);
            }
        }
    }

    /// Draw the per-viewport control overlay (sync, series, view mode, MPR
    /// plane, fit/zoom/rotate, window/level) on top of a viewport cell.
    fn viewport_controls_overlay(&mut self, ui: &mut egui::Ui, idx: usize, cell_rect: egui::Rect) {
        if idx >= self.viewports.len() || self.series_groups.is_empty() {
            return;
        }
        if self.wl_editor_open.len() <= idx {
            self.wl_editor_open.resize(idx + 1, false);
        }

        let margin = 6.0_f32;
        let row_h = 22.0_f32;
        let was_active = self.active_viewport;
        // Helpers below operate on the active viewport.
        self.active_viewport = idx;

        // ── Top-left: sync (own position) ─────────────────────────────────
        let top_left = egui::pos2(cell_rect.left() + margin, cell_rect.top() + margin);
        self.viewport_area(ui, ("vp_sync", idx), top_left, 320.0, |app, ui| {
            app.sync_controls_ui(ui, idx);
        });

        // ── Top-right: view mode + MPR plane (own position) ───────────────
        let top_right = egui::pos2(cell_rect.right() - margin, cell_rect.top() + margin);
        self.viewport_area(ui, ("vp_view", idx), top_right - egui::vec2(320.0, 0.0), 320.0, |app, ui| {
            app.view_mode_controls_ui(ui, idx);
        });

        // ── Bottom-right: window/level overlay + presets (own position) ───
        let wl_width = 210.0_f32;
        let wl_height = if self.wl_editor_open[idx] { 2.0 * row_h + 8.0 } else { row_h };
        let bottom_right = egui::pos2(
            cell_rect.right() - margin - wl_width,
            cell_rect.bottom() - margin - wl_height,
        );
        let mut toggle_wl = false;
        self.viewport_area(ui, ("vp_wl", idx), bottom_right, wl_width, |app, ui| {
            toggle_wl = app.window_level_overlay_ui(ui, idx);
        });
        if toggle_wl {
            self.wl_editor_open[idx] = !self.wl_editor_open[idx];
            self.viewports[idx].wl_dirty = true;
        }

        // ── Bottom-left: series selector on its own (own position) ────────
        let series_width = (cell_rect.width() * 0.55).clamp(180.0, 360.0);
        let bottom_left = egui::pos2(
            cell_rect.left() + margin,
            cell_rect.bottom() - margin - row_h,
        );
        self.viewport_area(ui, ("vp_series", idx), bottom_left, series_width, |app, ui| {
            app.series_selector_ui(ui, idx, series_width);
        });

        // ── Middle-left: fit / zoom / rotate (own position) ───────────────
        let fit_width = 250.0_f32;
        let fit_pos = egui::pos2(
            cell_rect.left() + margin,
            (cell_rect.center().y - row_h * 1.5).max(cell_rect.top() + 3.0 * row_h),
        );
        self.viewport_area(ui, ("vp_fit", idx), fit_pos, fit_width, |app, ui| {
            app.fit_zoom_rotate_ui(ui, idx);
        });

        self.active_viewport = was_active;
    }

    /// Run an overlay `egui::Area` at a fixed position, clipped to the cell.
    fn viewport_area(
        &mut self,
        ui: &mut egui::Ui,
        id: (&'static str, usize),
        pos: egui::Pos2,
        max_width: f32,
        add: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let cell = ui.clip_rect();
        egui::Area::new(egui::Id::new(id))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(cell);
                let was_active = self.active_viewport;
                add(self, ui);
                self.active_viewport = was_active;
                let _ = max_width;
            });
    }

    /// Sync group controls (checkbox + group chooser).
    fn sync_controls_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        let overlay_width = 200.0_f32;
        egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.set_max_width(overlay_width);
                ui.horizontal(|ui| {
                    let mut linked = self.viewports[idx].sync_group.is_some();
                    let resp = ui
                        .checkbox(&mut linked, "🔗 Sync")
                        .on_hover_text(
                            "Link this viewport into a sync group: navigating any member moves \
                             the others to the matching patient-space slice.",
                        );
                    if resp.clicked() {
                        if linked {
                            let group = self
                                .sync_groups()
                                .into_iter()
                                .max()
                                .unwrap_or_else(|| self.next_sync_group());
                            self.viewports[idx].sync_group = Some(group);
                        } else {
                            self.viewports[idx].sync_group = None;
                        }
                        self.align_active_to_group(ui.ctx());
                    }
                    if let Some(group) = self.viewports[idx].sync_group {
                        let mut chosen = group;
                        let mut new_group = false;
                        egui::ComboBox::from_id_salt(("sync_group", idx))
                            .selected_text(format!("Group {}", group + 1))
                            .show_ui(ui, |ui| {
                                for g in self.sync_groups().clone() {
                                    ui.selectable_value(&mut chosen, g, format!("Group {}", g + 1));
                                }
                                if ui.selectable_label(false, "＋ New").clicked() {
                                    new_group = true;
                                }
                            });
                        if new_group {
                            let ng = self.next_sync_group();
                            self.viewports[idx].sync_group = Some(ng);
                            self.align_active_to_group(ui.ctx());
                        } else if chosen != group {
                            self.viewports[idx].sync_group = Some(chosen);
                            self.align_active_to_group(ui.ctx());
                        }
                    }
                });
            });
    }

    /// View mode (Stack/MPR) and MPR plane selection.
    fn view_mode_controls_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let previous_view_mode = self.viewports[idx].view_mode;
                    ui.selectable_value(&mut self.viewports[idx].view_mode, ViewMode::Stack, "Stack");
                    ui.add_enabled_ui(self.mpr_available(), |ui| {
                        ui.selectable_value(&mut self.viewports[idx].view_mode, ViewMode::Mpr, "MPR");
                    });
                    if self.viewports[idx].view_mode == ViewMode::Mpr {
                        let previous_plane = self.viewports[idx].mpr_plane;
                        for plane in [MprPlane::Axial, MprPlane::Coronal, MprPlane::Sagittal] {
                            ui.selectable_value(
                                &mut self.viewports[idx].mpr_plane,
                                plane,
                                match plane {
                                    MprPlane::Axial => "Ax",
                                    MprPlane::Coronal => "Cor",
                                    MprPlane::Sagittal => "Sag",
                                },
                            );
                        }
                        if previous_plane != self.viewports[idx].mpr_plane {
                            self.viewports[idx].wl_dirty = true;
                        }
                    }
                    // Switching Stack <-> MPR keeps the current window/level
                    // preset; only changing the series resets it.
                    if previous_view_mode != self.viewports[idx].view_mode {
                        self.viewports[idx].wl_dirty = true;
                    }
                });
            });
    }

    /// Fit / zoom / rotate controls.
    fn fit_zoom_rotate_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let zoom = self.viewports[idx].zoom;
                    if ui.small_button("Fit").clicked() {
                        self.viewports[idx].zoom = 1.0;
                        self.viewports[idx].rotation_degrees = 0.0;
                        self.viewports[idx].rotation_drag_last_pos = None;
                        self.viewports[idx].pan = Vec2::ZERO;
                    }
                    if ui.small_button("+").clicked() {
                        self.viewports[idx].zoom = (zoom * 1.25).min(20.0);
                    }
                    if ui.small_button("–").clicked() {
                        self.viewports[idx].zoom = (zoom / 1.25).max(0.05);
                    }
                    ui.label(format!("{:.0}%", self.viewports[idx].zoom * 100.0));
                    if ui.small_button("⟲").on_hover_text("Rotate 90° CCW").clicked() {
                        self.viewports[idx].rotation_degrees =
                            (self.viewports[idx].rotation_degrees - 90.0).rem_euclid(360.0);
                    }
                    if ui.small_button("⟳").on_hover_text("Rotate 90° CW").clicked() {
                        self.viewports[idx].rotation_degrees =
                            (self.viewports[idx].rotation_degrees + 90.0).rem_euclid(360.0);
                    }
                    ui.label(format!("{:.0}°", self.viewports[idx].rotation_degrees));
                });
            });
    }

    /// Window/level status; double-click toggles the editable WC/WW fields.
    /// Presets sit in the same bottom-right cluster.
    fn window_level_overlay_ui(&mut self, ui: &mut egui::Ui, idx: usize) -> bool {
        let mut toggle = false;
        let windowing_enabled = match self.viewports[idx].view_mode {
            ViewMode::Stack => self
                .image_for(idx)
                .map(|image| image.raw_image.is_grayscale16())
                .unwrap_or(false),
            ViewMode::Mpr => self
                .mpr_series_for(idx)
                .and_then(|s| self.mpr_volumes.get(s))
                .and_then(|o| o.as_ref())
                .is_some(),
        };
        if !windowing_enabled {
            return false;
        }
        let editing = self.wl_editor_open.get(idx).copied().unwrap_or(false);
        egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let text = format!(
                        "W/L: {:.0}/{:.0}",
                        self.viewports[idx].window_center, self.viewports[idx].window_width
                    );
                    let label = ui
                        .add(
                            egui::Label::new(text)
                                .sense(egui::Sense::click())
                                .selectable(false),
                        )
                        .on_hover_text("Double-click to edit window/level");
                    if label.double_clicked() {
                        toggle = true;
                    }

                    let presets_label = match self.active_modality().as_deref() {
                        Some(m) if !m.is_empty() => format!("Presets ({m})"),
                        _ => "Presets".to_string(),
                    };
                    ui.menu_button(presets_label, |ui| {
                        let modality = self.active_modality();
                        let is_ct = modality.as_deref() == Some("CT");
                        let default_wl = self.active_default_wc_ww();
                        if is_ct {
                            ui.label("CT (HU)");
                            for (name, wc, ww) in CT_WL_PRESETS {
                                if ui.button(*name).clicked() {
                                    self.viewports[idx].window_center = *wc;
                                    self.viewports[idx].window_width = (*ww).max(1.0);
                                    self.viewports[idx].wl_dirty = true;
                                    ui.close();
                                }
                            }
                            ui.separator();
                        }
                        if let Some((wc, ww)) = default_wl {
                            for (lbl, scale) in
                                [("Auto (full range)", 1.0f32), ("Narrow", 0.5), ("Wide", 2.0)]
                            {
                                if ui.button(lbl).clicked() {
                                    self.viewports[idx].window_center = wc;
                                    self.viewports[idx].window_width = (ww * scale).max(1.0);
                                    self.viewports[idx].wl_dirty = true;
                                    ui.close();
                                }
                            }
                        }
                    });
                });

                if editing {
                    ui.horizontal(|ui| {
                        ui.label("WC");
                        let wc_resp = ui.add(
                            egui::DragValue::new(&mut self.viewports[idx].window_center)
                                .speed(1.0),
                        );
                        ui.label("WW");
                        let ww_resp = ui.add(
                            egui::DragValue::new(&mut self.viewports[idx].window_width)
                                .speed(1.0)
                                .range(1.0..=f32::MAX),
                        );
                        if wc_resp.changed() || ww_resp.changed() {
                            self.viewports[idx].wl_dirty = true;
                            self.update_slice_view_for(idx, ui.ctx());
                        }
                    });
                }
            });
        toggle
    }

    /// Series selector for this viewport, on its own at the bottom.
    fn series_selector_ui(&mut self, ui: &mut egui::Ui, idx: usize, width: f32) {
        egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                let selected_text = self
                    .series_groups
                    .get(self.viewports[idx].current_series)
                    .map(|g| g.label.as_str())
                    .unwrap_or("(none)")
                    .to_string();
                egui::ComboBox::from_id_salt(("viewport_series", idx))
                    .selected_text(selected_text)
                    .width(width - 20.0)
                    .show_ui(ui, |ui| {
                        let mut selected_series = self.viewports[idx].current_series;
                        for (sidx, group) in self.series_groups.iter().enumerate() {
                            ui.selectable_value(
                                &mut selected_series,
                                sidx,
                                format!("{} ({} images)", group.label, group.image_indices.len()),
                            );
                        }
                        if selected_series != self.viewports[idx].current_series {
                            self.viewports[idx].current_series = selected_series;
                            self.viewports[idx].current_stack_slice = 0;
                            self.viewports[idx].current_axial_slice = 0;
                            self.viewports[idx].current_coronal_slice = 0;
                            self.viewports[idx].current_sagittal_slice = 0;
                            self.viewports[idx].pan = Vec2::ZERO;
                            self.refresh_mpr_volumes();
                            self.apply_default_window_for_current_series();
                            self.viewports[idx].wl_dirty = true;
                            self.update_slice_view_for(idx, ui.ctx());
                        }
                    });
            });
    }


    /// Rect a viewport's image is drawn into, based on the last render.
    fn viewport_display_rect(
        &self,
        width: usize,
        height: usize,
        physical_size: Vec2,
        mode: ViewMode,
    ) -> Option<egui::Rect> {
        let rect: egui::Rect = self
            .viewport_image_rects
            .get(self.active_viewport)
            .copied()
            .flatten()?;
        let img_w = width as f32;
        let img_h = height as f32;
        let vp = self.vp();
        let display = if mode == ViewMode::Mpr && !vp.scale_by_physical {
            let fit = (rect.width() / img_w).min(rect.height() / img_h);
            egui::vec2(img_w * fit * vp.zoom, img_h * fit * vp.zoom)
        } else {
            let fit = (rect.width() / physical_size.x).min(rect.height() / physical_size.y);
            egui::vec2(physical_size.x * fit * vp.zoom, physical_size.y * fit * vp.zoom)
        };
        let center = rect.center() + vp.pan;
        Some(egui::Rect::from_center_size(center, display))
    }

    fn update_current_slice_view(&mut self, ctx: &egui::Context) {
        let idx = self.active_viewport;
        self.update_slice_view_for(idx, ctx);
    }

    /// A patient-space point lying on the plane currently shown by `idx`.
    fn viewport_anchor_point(&self, idx: usize) -> Option<[f32; 3]> {
        let vp = self.viewports.get(idx)?;
        if vp.view_mode == ViewMode::Mpr {
            let volume = self.mpr_volumes.get(self.mpr_series_for(idx)?)?.as_ref()?;
            let (_, _, w_axis) = vp.mpr_plane.patient_axes();
            let axis = w_axis.index();
            let coord = volume.axis_coordinate(w_axis, self.mpr_slice_for(idx));
            let mut point = volume.patient_min;
            point[axis] = coord;
            Some(point)
        } else {
            self.image_for(idx)?.image_position_patient
        }
    }

    /// Index (within the series group) of the slice whose plane is closest to
    /// `point`, for a stack viewport.
    fn stack_index_for_point(&self, idx: usize, point: [f32; 3]) -> Option<usize> {
        let vp = self.viewports.get(idx)?;
        if vp.view_mode != ViewMode::Stack {
            return None;
        }
        let group = self.series_groups.get(vp.current_series)?;
        let first = self.images.get(*group.image_indices.first()?)?;
        let orient = first.image_orientation_patient?;
        let col_dir = [orient[0], orient[1], orient[2]];
        let row_dir = [orient[3], orient[4], orient[5]];
        let normal = normalize(cross(col_dir, row_dir))?;
        let target = dot(point, normal);
        let mut best = 0usize;
        let mut best_dist = f32::INFINITY;
        for (i, global) in group.image_indices.iter().enumerate() {
            let Some(img) = self.images.get(*global) else {
                continue;
            };
            let Some(pos) = img.image_position_patient else {
                continue;
            };
            let dist = (dot(pos, normal) - target).abs();
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        Some(best)
    }

    /// MPR slice index whose plane is closest to `point`, for an MPR viewport.
    fn mpr_index_for_point(&self, idx: usize, point: [f32; 3]) -> Option<usize> {
        let vp = self.viewports.get(idx)?;
        if vp.view_mode != ViewMode::Mpr {
            return None;
        }
        let volume = self.mpr_volumes.get(self.mpr_series_for(idx)?)?.as_ref()?;
        let (_, _, w_axis) = vp.mpr_plane.patient_axes();
        let len = volume.plane_len(vp.mpr_plane);
        if len == 0 {
            return None;
        }
        let axis = w_axis.index();
        let unit = w_axis.unit();
        let spacing = volume.axis_spacing(w_axis);
        let coord = dot(point, unit);
        let raw = ((coord - volume.patient_min[axis]) / spacing).round();
        Some(raw.max(0.0).min((len - 1) as f32) as usize)
    }

    /// Move every other viewport in `source`'s sync group to the patient-space
    /// position shown by `source`.
    fn sync_from(&mut self, source: usize, ctx: &egui::Context) {
        let Some(group) = self.viewports.get(source).and_then(|vp| vp.sync_group) else {
            return;
        };
        if self.viewports.len() < 2 {
            return;
        }
        let Some(anchor) = self.viewport_anchor_point(source) else {
            return;
        };

        let mut changed: Vec<usize> = Vec::new();
        for target in 0..self.viewports.len() {
            if target == source || self.viewports[target].sync_group != Some(group) {
                continue;
            }
            match self.viewports[target].view_mode {
                ViewMode::Stack => {
                    if let Some(new_index) = self.stack_index_for_point(target, anchor) {
                        if self.viewports[target].current_stack_slice != new_index {
                            self.viewports[target].current_stack_slice = new_index;
                            self.viewports[target].wl_dirty = true;
                            changed.push(target);
                        }
                    }
                }
                ViewMode::Mpr => {
                    if let Some(new_index) = self.mpr_index_for_point(target, anchor) {
                        if self.mpr_slice_for(target) != new_index {
                            self.set_mpr_slice_for(target, new_index);
                            self.viewports[target].wl_dirty = true;
                            changed.push(target);
                        }
                    }
                }
            }
        }

        // Rebuild the textures of the moved viewports immediately (the active
        // texture was already built by the navigation handler).
        let changed_any = !changed.is_empty();
        let saved = self.active_viewport;
        for target in changed {
            self.update_slice_view_for(target, ctx);
        }
        self.active_viewport = saved;
        if changed_any {
            ctx.request_repaint();
        }
    }

    /// Next unused sync group id.
    fn next_sync_group(&self) -> u32 {
        self.viewports
            .iter()
            .filter_map(|vp| vp.sync_group)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// Distinct sync groups currently in use (sorted).
    fn sync_groups(&self) -> Vec<u32> {
        let mut groups: Vec<u32> = self.viewports.iter().filter_map(|vp| vp.sync_group).collect();
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    /// Align the active viewport to the current position of its sync group.
    /// Called when the active viewport joins/changes a group.
    fn align_active_to_group(&mut self, ctx: &egui::Context) {
        let active = self.active_viewport;
        let Some(group) = self.viewports.get(active).and_then(|vp| vp.sync_group) else {
            return;
        };
        if let Some(src) =
            (0..self.viewports.len()).find(|&i| i != active && self.viewports[i].sync_group == Some(group))
        {
            self.sync_from(src, ctx);
        }
        self.last_navs = self.all_nav_snapshots();
    }

    fn viewport_nav(&self, idx: usize) -> NavSnapshot {
        let vp = &self.viewports[idx];
        NavSnapshot {
            series: vp.current_series,
            stack: vp.current_stack_slice,
            axial: vp.current_axial_slice,
            coronal: vp.current_coronal_slice,
            sagittal: vp.current_sagittal_slice,
            plane: vp.mpr_plane,
            mode: vp.view_mode,
        }
    }

    fn all_nav_snapshots(&self) -> Vec<NavSnapshot> {
        (0..self.viewports.len())
            .map(|i| self.viewport_nav(i))
            .collect()
    }

    /// Detect which viewport was navigated this frame and propagate it to the
    /// other members of its sync group. Called once per frame after rendering.
    ///
    /// A viewport counts as navigated when its slice index changed within the
    /// same series, plane and view mode (so switching series or plane does not
    /// drag the other viewports around). Only one source is propagated per
    /// frame; the last changed viewport wins.
    fn autosync_active(&mut self, ctx: &egui::Context) {
        if self.viewports.len() < 2 {
            self.last_navs = self.all_nav_snapshots();
            return;
        }
        if self.last_navs.len() != self.viewports.len() {
            self.last_navs = self.all_nav_snapshots();
            return;
        }

        let mut source: Option<usize> = None;
        for i in 0..self.viewports.len() {
            let cur = self.viewport_nav(i);
            let prev = self.last_navs[i];
            let navigated = prev.series == cur.series
                && prev.mode == cur.mode
                && prev.plane == cur.plane
                && (prev.stack != cur.stack
                    || prev.axial != cur.axial
                    || prev.coronal != cur.coronal
                    || prev.sagittal != cur.sagittal);
            if navigated {
                source = Some(i);
            }
        }

        if let Some(src) = source {
            self.sync_from(src, ctx);
        }
        // Re-baseline after propagation so the synced movement does not echo.
        self.last_navs = self.all_nav_snapshots();
    }
}

impl eframe::App for DicomViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Process any pending file load (first frame, or after Open dialog)
        if let Some((paths, record_recent)) = self.pending_load.take() {
            self.load_files(paths, record_recent, ui.ctx());
        }

        // ── Poll background loading thread ─────────────────────────────────
        self.poll_loading(ui.ctx());

        // Rebuild texture when window/level values change for active viewport
        if self.vp().wl_dirty {
            self.update_current_slice_view(ui.ctx());
        }

        // Drag-and-drop: detect hovered and dropped files
        ui.ctx().input(|i| {
            self.files_hovered = !i.raw.hovered_files.is_empty();
        });

        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if !dropped.is_empty() {
            self.files_hovered = false;
            self.load_files(dropped, true, ui.ctx());
        }

        // Clear the locked zoom anchor once the zoom-drag gesture ends.
        let zoom_dragging = ui.ctx().input(|i| {
            i.pointer.button_down(egui::PointerButton::Primary)
                && i.pointer.button_down(egui::PointerButton::Secondary)
        });
        if !zoom_dragging {
            for vp in &mut self.viewports {
                vp.zoom_drag_anchor = None;
            }
        }

        // Clear the rotation anchor as soon as the rotate button is released, so
        // that a new rotate gesture starts from the current pointer position and
        // does not jump to the angle of the previous gesture.
        let rotate_held = ui.ctx().input(|i| i.pointer.button_down(egui::PointerButton::Extra1));
        if !rotate_held {
            for vp in &mut self.viewports {
                vp.rotation_drag_last_pos = None;
            }
        }

        // Keyboard navigation: arrow keys (operates on active viewport)
        ui.ctx().input(|i| {
            if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowLeft) {
                let current = match self.vp().view_mode {
                    ViewMode::Stack => self.vp().current_stack_slice,
                    ViewMode::Mpr => self.current_mpr_slice(),
                };
                if current > 0 {
                    match self.vp().view_mode {
                        ViewMode::Stack => self.vp_mut().current_stack_slice -= 1,
                        ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                    }
                    self.vp_mut().wl_dirty = true;
                }
            }
            if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight) {
                let len = self.current_view_slice_len();
                let current = match self.vp().view_mode {
                    ViewMode::Stack => self.vp().current_stack_slice,
                    ViewMode::Mpr => self.current_mpr_slice(),
                };
                if current < len.saturating_sub(1) {
                    match self.vp().view_mode {
                        ViewMode::Stack => self.vp_mut().current_stack_slice += 1,
                        ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                    }
                    self.vp_mut().wl_dirty = true;
                }
            }
        });

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.series_groups.is_empty() {
                    if ui.button("📄 Open Files").clicked() {
                        if let Some(paths) = rfd::FileDialog::new()
                            .add_filter("DICOM", &["dcm", "DCM"])
                            .add_filter("DICOM", &["dicom", "DICOM"])
                            .add_filter("All Files", &["*"])
                            .pick_files()
                        {
                            self.pending_load = Some((paths, true));
                        }
                    }

                    if ui.button("📁 Open Folder").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.pending_load = Some((vec![folder], true));
                        }
                    }

                    // Recent folders menu
                    if !self.recent_folders.is_empty() {
                        ui.menu_button("Recent", |ui| {
                            for p in &self.recent_folders {
                                if ui.button(p.display().to_string()).clicked() {
                                    self.pending_load = Some((vec![p.clone()], true));
                                    ui.close();
                                }
                            }
                        });
                    }
                } else {
                    // Collapsed single-icon menu when files are already opened
                    ui.menu_button("📂", |ui| {
                        if ui.button("📄 Open Files").clicked() {
                            if let Some(paths) = rfd::FileDialog::new()
                                .add_filter("DICOM", &["dcm", "DCM"])
                                .add_filter("DICOM", &["dicom", "DICOM"])
                                .add_filter("All Files", &["*"])
                                .pick_files()
                            {
                                self.pending_load = Some((paths, true));
                                ui.close();
                            }
                        }
                        if ui.button("📁 Open Folder").clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                self.pending_load = Some((vec![folder], true));
                                ui.close();
                            }
                        }
                        if !self.recent_folders.is_empty() {
                            ui.separator();
                            ui.label("Recent:");
                            for p in &self.recent_folders {
                                if ui.button(p.display().to_string()).clicked() {
                                    self.pending_load = Some((vec![p.clone()], true));
                                    ui.close();
                                }
                            }
                        }
                    });
                }

                ui.separator();
                ui.toggle_value(&mut self.show_metadata, "ℹ Metadata");

                // Viewport selector + add/remove
                ui.horizontal(|ui| {
                    ui.label("Viewport:");
                    let mut vp_labels: Vec<String> = (0..self.viewports.len())
                        .map(|i| format!("Viewport {}", i + 1))
                        .collect();
                    let selected = self.active_viewport;
                    egui::ComboBox::from_id_salt("viewport_selector")
                        .selected_text(vp_labels.get(selected).cloned().unwrap_or_else(|| "Viewport 1".to_string()))
                        .show_ui(ui, |ui| {
                            for i in 0..self.viewports.len() {
                                ui.selectable_value(&mut (self.active_viewport), i, format!("{}", vp_labels[i]));
                            }
                        });
                    if ui.button("+").clicked() {
                        self.viewports.push(ViewportState::default());
                        self.viewport_textures.push(None);
                        self.active_viewport = self.viewports.len() - 1;
                    }
                    if self.viewports.len() > 1 && ui.button("–").clicked() {
                        let idx = self.active_viewport;
                        self.viewports.remove(idx);
                        if self.viewport_textures.len() > idx {
                            self.viewport_textures.remove(idx);
                        }
                        if self.active_viewport >= self.viewports.len() {
                            self.active_viewport = self.viewports.len().saturating_sub(1);
                        }
                    }
                });

                if !self.series_groups.is_empty() {
                    ui.separator();
                    ui.label("Series:");
                    let selected_text = self
                        .series_groups
                        .get(self.vp().current_series)
                        .map(|g| g.label.as_str())
                        .unwrap_or("(none)");
                    egui::ComboBox::from_id_salt("series_selector")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            let mut selected_series = self.vp().current_series;
                            for (idx, group) in self.series_groups.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    // Show thumbnail for the first image in the series if available
                                    if let Some(first_idx) = group.image_indices.first() {
                                        if let Some(img) = self.images.get(*first_idx) {
                                            if let Some(tex) = self.thumbnail_textures.get(&img.filename) {
                                                if ui
                                                    .add(egui::Button::new(egui::Image::new((tex.id(), egui::vec2(64.0, 64.0)))))
                                                    .clicked()
                                                {
                                                    selected_series = idx;
                                                }
                                            } else {
                                                ui.allocate_ui(egui::Vec2::new(64.0, 64.0), |ui| {
                                                    ui.label(egui::RichText::new(" "));
                                                });
                                            }
                                        }
                                    }
                                    ui.selectable_value(
                                        &mut selected_series,
                                        idx,
                                        format!("{} ({} images)", group.label, group.image_indices.len()),
                                    );
                                });
                            }
                            if selected_series != self.vp().current_series {
                                self.vp_mut().current_series = selected_series;
                                self.vp_mut().current_stack_slice = 0;
                                self.vp_mut().current_axial_slice = 0;
                                self.vp_mut().current_coronal_slice = 0;
                                self.vp_mut().current_sagittal_slice = 0;
                                self.vp_mut().pan = Vec2::ZERO;
                                self.refresh_mpr_volumes();
                                self.apply_default_window_for_current_series();
                                self.vp_mut().wl_dirty = true;
                            }
                        });

                    ui.separator();
                    let can_deface = self.active_series_is_grayscale16();
                    if ui
                        .add_enabled(can_deface, egui::Button::new("🙈 Deface"))
                        .on_hover_text(
                            "Blank facial voxels in this stack and open the result in a \
                             second viewport for direct comparison",
                        )
                        .clicked()
                    {
                        self.deface_active_series(ui.ctx());
                    }
                    if ui
                        .toggle_value(&mut self.deface_panel_open, "🎛 Align")
                        .on_hover_text(
                            "Manually align the defacing orientation and adjust depth/extent",
                        )
                        .changed()
                        && !self.deface_panel_open
                    {
                        // Closing the panel removes the preview overlay.
                        self.deface_preview = None;
                        self.redraw_all_viewports(ui.ctx());
                    }
                }

            });

            // Manual defacing alignment controls (below the toolbar).
            if self.deface_panel_open {
                self.deface_alignment_ui(ui);
            }

            if let Some(msg) = &self.deface_message {
                ui.label(
                    egui::RichText::new(msg)
                        .small()
                        .color(egui::Color32::from_rgb(140, 190, 255)),
                );
            }
        });

        // ── Metadata side panel ───────────────────────────────────────────────
        if self.active_series_len() > 0 {
            let Some(active_image) = self.metadata_image() else {
                return;
            };
            // Build metadata list from DICOM tags plus derived fields
            let mut metadata = active_image.metadata.clone();

            if let Some(study) = &active_image.study_uid {
                metadata.push(("Study Instance UID".to_string(), study.clone()));
            }
            metadata.push(("Series UID".to_string(), active_image.series_uid.clone()));

            if let Some(pos) = active_image.image_position_patient {
                metadata.push((
                    "Image Position (Patient)".to_string(),
                    format!("{:.4}, {:.4}, {:.4}", pos[0], pos[1], pos[2]),
                ));
            }
            if let Some(orient) = active_image.image_orientation_patient {
                metadata.push((
                    "Image Orientation (Patient)".to_string(),
                    format!("{:.6}, {:.6}, {:.6}, {:.6}, {:.6}, {:.6}", orient[0], orient[1], orient[2], orient[3], orient[4], orient[5]),
                ));
            }
            if let Some(sp) = active_image.pixel_spacing {
                metadata.push(("Pixel Spacing".to_string(), format!("{:.6} \\ {:.6}", sp[0], sp[1])));
            }
            if let Some(st) = active_image.slice_thickness {
                metadata.push(("Slice Thickness".to_string(), format!("{:.6}", st)));
            }
            if let Some(sbs) = active_image.spacing_between_slices {
                metadata.push(("Spacing Between Slices".to_string(), format!("{:.6}", sbs)));
            }

            if !metadata.is_empty() {
                let mut show_metadata = self.show_metadata;
                egui::Panel::right("metadata_panel")
                    .min_size(180.0)
                    .max_size(300.0)
                    .default_size(220.0)
                    .resizable(true)
                    .show_collapsible(ui, &mut show_metadata, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Metadata");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("»")
                                        .on_hover_text("Collapse metadata panel")
                                        .clicked()
                                    {
                                        self.show_metadata = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            egui::Grid::new("meta_grid")
                                .num_columns(2)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (label, value) in &metadata {
                                        ui.strong(label);
                                        ui.label(value);
                                        ui.end_row();
                                    }
                                });
                        });
                    });
                if !self.show_metadata {
                    show_metadata = false;
                }
                self.show_metadata = show_metadata;
            }
        }

        // ── Image panel ───────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.error.clone() {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 60, 60),
                        format!("⚠ {}", err),
                    );
                });
                return;
            }

            // Show drop zone if files are being hovered
            if self.files_hovered {
                let rect = ui.available_rect_before_wrap();
                // Draw a semi-transparent overlay to indicate drop zone
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(100, 150, 255, 32),
                );
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(100.0);
                        ui.heading("📥 Drop DICOM files here");
                        ui.add_space(100.0);
                    });
                });
                return;
            }

                let active_tex = self.texture_for_active();
                // If multiple viewports exist, render them in a simple grid. Clicking a cell makes it active.
                if self.viewports.len() > 1 {
                    let n = self.viewports.len();
                    let cols = (n as f32).sqrt().ceil() as usize;
                    let rows = (n + cols - 1) / cols;
                    let rect = ui.available_rect_before_wrap();
                    let cell_w = rect.width() / (cols as f32);
                    let cell_h = rect.height() / (rows as f32);
                    let mut vp_anchors: Vec<(usize, egui::Pos2, Option<String>, egui::Rect)> = Vec::new();
                    for r in 0..rows {
                        for c in 0..cols {
                            let idx = r * cols + c;
                            if idx >= n {
                                continue;
                            }
                            let min = egui::pos2(rect.left() + c as f32 * cell_w, rect.top() + r as f32 * cell_h);
                            let cell_rect = egui::Rect::from_min_size(min, egui::vec2(cell_w, cell_h));
                            let response = ui.allocate_rect(cell_rect, egui::Sense::click_and_drag());

                            // Draw background
                            let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(40));
                            let painter = ui.painter();
                            painter.line_segment([cell_rect.left_top(), cell_rect.right_top()], stroke);
                            painter.line_segment([cell_rect.right_top(), cell_rect.right_bottom()], stroke);
                            painter.line_segment([cell_rect.right_bottom(), cell_rect.left_bottom()], stroke);
                            painter.line_segment([cell_rect.left_bottom(), cell_rect.left_top()], stroke);

                                // Draw texture or placeholder
                            if let Some(tex) = self.viewport_textures.get(idx).and_then(|o| o.clone()) {
                                let tex_size = tex.size();
                                let img_w = tex_size[0] as f32;
                                let img_h = tex_size[1] as f32;
                                let vp = &self.viewports[idx];
                                let physical_size = vp.displayed_physical_size.unwrap_or_else(|| egui::vec2(img_w, img_h));
                                // Choose scaling mode according to per-viewport flag
                                let display = if vp.view_mode == ViewMode::Mpr && !vp.scale_by_physical {
                                    // Pixel-based scaling
                                    let fit = (cell_w / img_w).min(cell_h / img_h);
                                    egui::vec2(img_w * fit * vp.zoom, img_h * fit * vp.zoom)
                                } else {
                                    // Physical-size based scaling
                                    let fit = (cell_w / physical_size.x).min(cell_h / physical_size.y);
                                    egui::vec2(physical_size.x * fit * vp.zoom, physical_size.y * fit * vp.zoom)
                                };
                                let center = cell_rect.center() + vp.pan;
                                let angle_rad = vp.rotation_degrees.to_radians();
                                let painter = ui.painter();
                                let clipped = painter.with_clip_rect(cell_rect);
                                paint_rotated_texture(&clipped, tex.id(), center, display, angle_rad);

                                // Draw per-viewport vertical slice indicator on the right side of the cell
                                    let total_slices = if vp.view_mode == ViewMode::Stack {
                                        self.series_groups
                                            .get(vp.current_series)
                                            .map(|g| g.image_indices.len())
                                            .unwrap_or(0)
                                    } else {
                                        self.mpr_volumes
                                            .get(self.mpr_series_for(idx).unwrap_or(usize::MAX))
                                            .and_then(|o| o.as_ref())
                                            .map(|volume| volume.plane_len(vp.mpr_plane))
                                            .unwrap_or(0)
                                    };
                                    if total_slices > 1 {
                                        let current_idx = if vp.view_mode == ViewMode::Stack {
                                            vp.current_stack_slice
                                        } else {
                                            match vp.mpr_plane {
                                                MprPlane::Axial => vp.current_axial_slice,
                                                MprPlane::Coronal => vp.current_coronal_slice,
                                                MprPlane::Sagittal => vp.current_sagittal_slice,
                                            }
                                        } as usize;

                                        let track_width = (cell_w * 0.06).max(8.0_f32);
                                        let padding = (cell_w * 0.02).max(4.0_f32);
                                        let track_rect = egui::Rect::from_min_size(
                                            egui::pos2(cell_rect.right() - padding - track_width, cell_rect.top() + padding),
                                            egui::vec2(track_width, cell_rect.height() - padding * 2.0),
                                        );

                                        // Draw track background (clipped to cell)
                                        let track_painter = painter.with_clip_rect(cell_rect);
                                        track_painter.rect_filled(
                                            track_rect,
                                            track_width * 0.5,
                                            egui::Color32::from_rgba_unmultiplied(60, 60, 60, 120),
                                        );

                                        // Compute thumb position and size (proportional)
                                        let thumb_h = (track_rect.height() / (total_slices as f32)).max(6.0);
                                        let available = track_rect.height() - thumb_h;
                                        let t = if total_slices > 1 {
                                            (current_idx as f32) / ((total_slices - 1) as f32)
                                        } else {
                                            0.0
                                        };
                                        let thumb_y = track_rect.top() + t * available;
                                        let thumb_rect = egui::Rect::from_min_size(
                                            egui::pos2(track_rect.left(), thumb_y),
                                            egui::vec2(track_rect.width(), thumb_h),
                                        );

                                        // Draw thumb
                                        track_painter.rect_filled(
                                            thumb_rect,
                                            6.0,
                                            egui::Color32::from_rgb(200, 200, 200),
                                        );

                                        // Thin shadow/border (clipped)
                                        track_painter.rect_filled(
                                            thumb_rect.shrink(0.5),
                                            6.0,
                                            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 80),
                                        );

                                        // Click / drag anywhere on the track to navigate slices.
                                        let scroll_resp = ui.interact(
                                            track_rect,
                                            egui::Id::new(("slice_scroll_track", idx)),
                                            egui::Sense::click_and_drag(),
                                        );
                                        if scroll_resp.hovered() {
                                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                                        }
                                        if scroll_resp.dragged() || scroll_resp.clicked() {
                                            if let Some(pointer) = scroll_resp.interact_pointer_pos() {
                                                let t = ((pointer.y - track_rect.top())
                                                    / track_rect.height())
                                                    .clamp(0.0, 1.0);
                                                let new_idx =
                                                    (t * (total_slices - 1) as f32).round() as usize;
                                                let plane = self.viewports[idx].mpr_plane;
                                                let changed =
                                                    match self.viewports[idx].view_mode {
                                                        ViewMode::Stack => {
                                                            if self.viewports[idx].current_stack_slice
                                                                != new_idx
                                                            {
                                                                self.viewports[idx]
                                                                    .current_stack_slice = new_idx;
                                                                true
                                                            } else {
                                                                false
                                                            }
                                                        }
                                                        ViewMode::Mpr => {
                                                            let current = match plane {
                                                                MprPlane::Axial => self.viewports[idx]
                                                                    .current_axial_slice,
                                                                MprPlane::Coronal => self.viewports[idx]
                                                                    .current_coronal_slice,
                                                                MprPlane::Sagittal => self.viewports[idx]
                                                                    .current_sagittal_slice,
                                                            };
                                                            if current != new_idx {
                                                                match plane {
                                                                    MprPlane::Axial => self.viewports[idx]
                                                                        .current_axial_slice = new_idx,
                                                                    MprPlane::Coronal => self.viewports[idx]
                                                                        .current_coronal_slice = new_idx,
                                                                    MprPlane::Sagittal => self.viewports[idx]
                                                                        .current_sagittal_slice = new_idx,
                                                                }
                                                                true
                                                            } else {
                                                                false
                                                            }
                                                        }
                                                    };
                                                if changed {
                                                    self.active_viewport = idx;
                                                    self.viewports[idx].wl_dirty = true;
                                                    self.update_current_slice_view(ui.ctx());
                                                }
                                            }
                                        }

                                        // Draw slice count small label (clipped)
                                        let label = format!("{}/{}", current_idx + 1, total_slices);
                                        let text_pos = egui::pos2(track_rect.left() - 6.0, thumb_rect.center().y - 8.0);
                                        track_painter.text(
                                            text_pos,
                                            egui::Align2::RIGHT_CENTER,
                                            label,
                                            egui::FontId::default(),
                                            egui::Color32::WHITE,
                                        );
                                    }
                            } else {
                                ui.painter().text(
                                    cell_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "No image",
                                    egui::FontId::default(),
                                    egui::Color32::LIGHT_GRAY,
                                );
                            }

                            // Highlight active viewport
                        if idx == self.active_viewport {
                            let stroke = egui::Stroke::new(2.0, egui::Color32::LIGHT_GREEN);
                            let painter = ui.painter();
                            let r = cell_rect.shrink(4.0);
                            painter.line_segment([r.left_top(), r.right_top()], stroke);
                            painter.line_segment([r.right_top(), r.right_bottom()], stroke);
                            painter.line_segment([r.right_bottom(), r.left_bottom()], stroke);
                            painter.line_segment([r.left_bottom(), r.left_top()], stroke);
                        }

                            if response.clicked() {
                                if self.active_viewport != idx {
                                    self.active_viewport = idx;
                                    // Refresh the toolbar (window/level presets etc.)
                                    // immediately so it reflects the new active viewport.
                                    ui.ctx().request_repaint();
                                }
                            }

                            // Record the image rect before the overlay so its
                            // (possibly rebuilt) texture can be recomputed.
                            if let Some(tex) = self.viewport_textures.get(idx).and_then(|o| o.clone()) {
                                let tex_size = tex.size();
                                let vp = &self.viewports[idx];
                                let physical_size = vp
                                    .displayed_physical_size
                                    .unwrap_or_else(|| egui::vec2(tex_size[0] as f32, tex_size[1] as f32));
                                let display = if vp.view_mode == ViewMode::Mpr && !vp.scale_by_physical {
                                    let fit = (cell_w / tex_size[0] as f32).min(cell_h / tex_size[1] as f32);
                                    egui::vec2(tex_size[0] as f32 * fit * vp.zoom, tex_size[1] as f32 * fit * vp.zoom)
                                } else {
                                    let fit = (cell_w / physical_size.x).min(cell_h / physical_size.y);
                                    egui::vec2(physical_size.x * fit * vp.zoom, physical_size.y * fit * vp.zoom)
                                };
                                if self.viewport_image_rects.len() <= idx {
                                    self.viewport_image_rects.resize(idx + 1, None);
                                }
                                self.viewport_image_rects[idx] =
                                    Some(egui::Rect::from_center_size(cell_rect.center() + vp.pan, display));
                            }

                            // Per-viewport controls drawn on top of this cell.
                            self.viewport_controls_overlay(ui, idx, cell_rect);

                            // record slice anchor point for cross-references (position along the displayed image)
                            if let Some(_tex) = self.viewport_textures.get(idx).and_then(|o| o.clone()) {
                                let vp = &self.viewports[idx];
                                // determine study id for this viewport's series (use first image in the series)
                                let study_id = self.series_groups.get(vp.current_series).and_then(|g| g.image_indices.first()).and_then(|first_idx| self.images.get(*first_idx)).and_then(|img| img.study_uid.clone());

                                // compute normalized slice position t in [0,1]
                                let mut t_opt: Option<f32> = None;
                                if vp.view_mode == ViewMode::Stack {
                                    if let Some(group) = self.series_groups.get(vp.current_series) {
                                        let mut positions: Vec<f32> = group
                                            .image_indices
                                            .iter()
                                            .filter_map(|&gi| self.images.get(gi).map(|img| img.slice_position()))
                                            .collect();
                                        if !positions.is_empty() {
                                            let minp = positions.iter().cloned().fold(f32::INFINITY, f32::min);
                                            let maxp = positions.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                                            let cur_idx = vp.current_stack_slice.min(group.image_indices.len().saturating_sub(1));
                                            if let Some(img) = self.images.get(group.image_indices[cur_idx]) {
                                                let pos = img.slice_position();
                                                if (maxp - minp).abs() > 1e-6 {
                                                    t_opt = Some(((pos - minp) / (maxp - minp)).clamp(0.0, 1.0));
                                                } else if group.image_indices.len() > 1 {
                                                    t_opt = Some(cur_idx as f32 / (group.image_indices.len() - 1) as f32);
                                                } else {
                                                    t_opt = Some(0.5);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    if let Some(volume) = self
                                        .mpr_series_for(idx)
                                        .and_then(|s| self.mpr_volumes.get(s))
                                        .and_then(|o| o.as_ref())
                                    {
                                        let len = volume.plane_len(vp.mpr_plane) as usize;
                                        if len > 1 {
                                            let cur = match vp.mpr_plane {
                                                MprPlane::Axial => vp.current_axial_slice,
                                                MprPlane::Coronal => vp.current_coronal_slice,
                                                MprPlane::Sagittal => vp.current_sagittal_slice,
                                            } as usize;
                                            t_opt = Some((cur as f32 / (len - 1) as f32).clamp(0.0, 1.0));
                                        }
                                    }
                                }

                                if let Some(t) = t_opt {
                                    // `display` and `center` were computed earlier for this viewport rendering
                                    let tex_size = self.viewport_textures.get(idx).and_then(|o| o.as_ref()).map(|t| t.size());
                                    // fallback: use cell_rect if texture size not available
                                    let display_h = if let Some(sz) = tex_size { sz[1] as f32 } else { cell_rect.height() };
                                    // We can reuse the previously computed `display` if available by recomputing similarly
                                    let img_w = tex_size.map(|s| s[0] as f32).unwrap_or(cell_rect.width());
                                    let img_h = tex_size.map(|s| s[1] as f32).unwrap_or(cell_rect.height());
                                    let physical_size = vp.displayed_physical_size.unwrap_or_else(|| egui::vec2(img_w, img_h));
                                    let display = if vp.view_mode == ViewMode::Mpr && !vp.scale_by_physical {
                                        let fit = (cell_rect.width() / img_w).min(cell_rect.height() / img_h);
                                        egui::vec2(img_w * fit * vp.zoom, img_h * fit * vp.zoom)
                                    } else {
                                        let fit = (cell_rect.width() / physical_size.x).min(cell_rect.height() / physical_size.y);
                                        egui::vec2(physical_size.x * fit * vp.zoom, physical_size.y * fit * vp.zoom)
                                    };
                                    let center = cell_rect.center() + vp.pan;
                                    let top = center.y - display.y * 0.5;
                                    let y = top + t * display.y;
                                    let anchor = egui::pos2(center.x, y);
                                    // anchor debug log removed
                                    vp_anchors.push((idx, anchor, study_id, cell_rect));
                                }
                            }

                            // Per-cell scroll: change slice (if stack/MPR) or zoom (single image)
                            let scroll_delta = ui.ctx().input(|i| i.smooth_scroll_delta.y);
                            if response.hovered() && scroll_delta != 0.0 {
                                let prev_active = self.active_viewport;
                                self.active_viewport = idx;
                                if !self.wheel_step_slices(idx, scroll_delta, ui.ctx()) {
                                    if self.current_view_slice_len() <= 1 {
                                        let new_zoom = (self.vp().zoom
                                            * (scroll_delta * 0.004).exp())
                                        .clamp(0.05, 20.0);
                                        if let Some(anchor) = response.hover_pos() {
                                            self.zoom_towards(new_zoom, anchor, cell_rect.center());
                                        } else {
                                            self.vp_mut().zoom = new_zoom;
                                        }
                                    }
                                }
                                self.active_viewport = prev_active;
                            }

                            // Per-cell double-click: reset view for that viewport
                            if response.double_clicked() {
                                let prev_active = self.active_viewport;
                                self.active_viewport = idx;
                                self.vp_mut().zoom = 1.0;
                                self.vp_mut().rotation_degrees = 0.0;
                                self.vp_mut().rotation_drag_last_pos = None;
                                self.vp_mut().pan = Vec2::ZERO;
                                self.apply_default_window_for_current_series();
                                self.vp_mut().wl_dirty = true;
                                self.update_current_slice_view(ui.ctx());
                                self.active_viewport = prev_active;
                            }

                            // Per-cell drag interactions (pan / rotate / window/level / slice)
                            let (left_down, right_down, middle_down, side1_down) = ui.ctx().input(|i| {
                                (
                                    i.pointer.button_down(egui::PointerButton::Primary),
                                    i.pointer.button_down(egui::PointerButton::Secondary),
                                    i.pointer.button_down(egui::PointerButton::Middle),
                                    i.pointer.button_down(egui::PointerButton::Extra1),
                                )
                            });

                            if response.hovered() {
                                let prev_active = self.active_viewport;
                                self.active_viewport = idx;

                                // Both left and right -> zoom (anchor locked at gesture start)
                                if left_down && right_down {
                                    let center = cell_rect.center();
                                    let anchor = match self.vp().zoom_drag_anchor {
                                        Some(a) => a,
                                        None => {
                                            let a = response.hover_pos().unwrap_or(center);
                                            let z = self.vp().zoom;
                                            self.vp_mut().zoom_drag_anchor = Some(a);
                                            self.vp_mut().zoom_drag_start_zoom = z;
                                            a
                                        }
                                    };
                                    // Zoom from total vertical displacement (frame-rate independent).
                                    if let Some(pointer) = response.interact_pointer_pos() {
                                        let factor = ((pointer.y - anchor.y) * 0.007).exp();
                                        let new_zoom = (self.vp().zoom_drag_start_zoom * factor)
                                            .clamp(0.05, 20.0);
                                        self.zoom_towards(new_zoom, anchor, center);
                                    }
                                }
                                // Middle drag -> slice navigation when multiple
                                else if middle_down && self.current_view_slice_len() > 1 {
                                    let delta = response.drag_delta();
                                    let current = match self.vp().view_mode {
                                        ViewMode::Stack => self.vp().current_stack_slice,
                                        ViewMode::Mpr => self.current_mpr_slice(),
                                    };
                                    if delta.y > 2.0 && current > 0 {
                                        match self.vp().view_mode {
                                            ViewMode::Stack => self.vp_mut().current_stack_slice -= 1,
                                            ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                                        }
                                        self.vp_mut().wl_dirty = true;
                                        self.update_current_slice_view(ui.ctx());
                                    } else if delta.y < -2.0 && current < self.current_view_slice_len() - 1 {
                                        match self.vp().view_mode {
                                            ViewMode::Stack => self.vp_mut().current_stack_slice += 1,
                                            ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                                        }
                                        self.vp_mut().wl_dirty = true;
                                        self.update_current_slice_view(ui.ctx());
                                    }
                                }
                                // Side mouse -> rotate
                                else if side1_down {
                                    let image_center = cell_rect.center() + self.vp().pan;
                                    let pointer_pos = ui.ctx().input(|i| i.pointer.interact_pos());
                                    if let Some(current_pos) = pointer_pos {
                                        if let Some(last_pos) = self.vp().rotation_drag_last_pos {
                                            let last_vec = last_pos - image_center;
                                            let current_vec = current_pos - image_center;
                                            let last_len2 = last_vec.length_sq();
                                            let current_len2 = current_vec.length_sq();
                                            if last_len2 > 9.0 && current_len2 > 9.0 {
                                                let last_angle = last_vec.y.atan2(last_vec.x);
                                                let current_angle = current_vec.y.atan2(current_vec.x);
                                                let mut delta_angle = current_angle - last_angle;
                                                while delta_angle > std::f32::consts::PI {
                                                    delta_angle -= 2.0 * std::f32::consts::PI;
                                                }
                                                while delta_angle < -std::f32::consts::PI {
                                                    delta_angle += 2.0 * std::f32::consts::PI;
                                                }
                                                self.vp_mut().rotation_degrees = (self.vp().rotation_degrees + delta_angle.to_degrees()).rem_euclid(360.0);
                                            }
                                        }
                                        self.vp_mut().rotation_drag_last_pos = Some(current_pos);
                                    } else {
                                        self.vp_mut().rotation_drag_last_pos = None;
                                    }
                                }
                                // Left drag -> pan
                                else if response.dragged_by(egui::PointerButton::Primary) && !right_down {
                                    self.vp_mut().pan += response.drag_delta();
                                }
                                // Right drag -> window/level
                                else if response.dragged_by(egui::PointerButton::Secondary) && !left_down {
                                    let windowing_enabled = match self.vp().view_mode {
                                        ViewMode::Stack => self
                                            .images
                                            .get(
                                                *self
                                                    .series_groups
                                                    .get(self.vp().current_series)
                                                    .and_then(|g| g.image_indices.first())
                                                    .unwrap_or(&0),
                                            )
                                            .map(|image| image.raw_image.is_grayscale16())
                                            .unwrap_or(false),
                                        ViewMode::Mpr => self.active_mpr_volume().is_some(),
                                    };
                                    if windowing_enabled {
                                        let delta = response.drag_delta();
                                        let ww_scale = 2.0_f32;
                                        let wc_scale = 2.0_f32;
                                        self.vp_mut().window_width = (self.vp().window_width + delta.x * ww_scale).max(1.0);
                                        self.vp_mut().window_center += -delta.y * wc_scale;
                                        self.vp_mut().wl_dirty = true;
                                        self.update_current_slice_view(ui.ctx());
                                    }
                                }

                                self.active_viewport = prev_active;
                            }

                        }
                    }

                    // Draw cross-reference lines between viewports that belong to the same study
                    if !vp_anchors.is_empty() {
                        let painter = ui.painter();
                        // debug anchors removed
                        for i in 0..vp_anchors.len() {
                                for j in (i + 1)..vp_anchors.len() {
                                let (ia, anchor_a, study_a, rect_a) = &vp_anchors[i];
                                let (ib, anchor_b, study_b, rect_b) = &vp_anchors[j];
                                if let (Some(s1), Some(s2)) = (study_a, study_b) {
                                    if !s1.is_empty() && s1 == s2 {
                                        // compute plane geometry for both viewports
                                        if let (Some((orig_a, n_a, col_a, row_a, col_sp_a, row_sp_a, w_a, h_a)),
                                                Some((orig_b, n_b, col_b, row_b, col_sp_b, row_sp_b, w_b, h_b))) =
                                            (self.viewport_plane(*ia), self.viewport_plane(*ib))
                                        {
                                            // intersect planes
                                            if let Some((p0, dir)) = Self::intersect_planes(n_a, orig_a, n_b, orig_b) {
                                                // for target B, find t range where projected (u,v) within image bounds
                                                let a_u = dot(sub(p0, orig_b), col_b) / col_sp_b;
                                                let b_u = dot(dir, col_b) / col_sp_b;
                                                let a_v = dot(sub(p0, orig_b), row_b) / row_sp_b;
                                                let b_v = dot(dir, row_b) / row_sp_b;

                                                let mut t_min = f32::NEG_INFINITY;
                                                let mut t_max = f32::INFINITY;

                                                // u bounds [0, w_b-1]
                                                let ub0 = 0.0_f32;
                                                let ub1 = (w_b as f32) - 1.0;
                                                if b_u.abs() < 1e-6 {
                                                    if a_u < ub0 || a_u > ub1 {
                                                        continue;
                                                    }
                                                } else {
                                                    let t1 = (ub0 - a_u) / b_u;
                                                    let t2 = (ub1 - a_u) / b_u;
                                                    let (ta, tb) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
                                                    t_min = t_min.max(ta);
                                                    t_max = t_max.min(tb);
                                                }

                                                // v bounds [0, h_b-1]
                                                let vb0 = 0.0_f32;
                                                let vb1 = (h_b as f32) - 1.0;
                                                if b_v.abs() < 1e-6 {
                                                    if a_v < vb0 || a_v > vb1 {
                                                        continue;
                                                    }
                                                } else {
                                                    let t1 = (vb0 - a_v) / b_v;
                                                    let t2 = (vb1 - a_v) / b_v;
                                                    let (ta, tb) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
                                                    t_min = t_min.max(ta);
                                                    t_max = t_max.min(tb);
                                                }

                                                if t_min <= t_max && t_min.is_finite() && t_max.is_finite() {
                                                    // compute endpoints in patient space
                                                    let p_start = add(p0, scale(dir, t_min));
                                                    let p_end = add(p0, scale(dir, t_max));

                                                    // project into B pixel coords then to screen
                                                    let (u0, v0) = self.project_point_to_pixel(orig_b, col_b, row_b, col_sp_b, row_sp_b, p_start);
                                                    let (u1, v1) = self.project_point_to_pixel(orig_b, col_b, row_b, col_sp_b, row_sp_b, p_end);
                                                    // convert to screen positions inside the stored cell rect for ib
                                                    let cell_rect = *rect_b;
                                                    let vp_b = &self.viewports[*ib];
                                                    let screen_p0 = self.pixel_to_screen(u0, v0, w_b as f32, h_b as f32, cell_rect, vp_b);
                                                    let screen_p1 = self.pixel_to_screen(u1, v1, w_b as f32, h_b as f32, cell_rect, vp_b);
                                                    // draw endpoint markers for debugging (only if finite) clipped to target cell B
                                                    let clipped_painter_b = painter.with_clip_rect(cell_rect);
                                                    if screen_p0.x.is_finite() && screen_p0.y.is_finite() {
                                                        clipped_painter_b.circle_filled(screen_p0, 3.0, egui::Color32::from_rgba_unmultiplied(220, 40, 40, 220));
                                                    }
                                                    if screen_p1.x.is_finite() && screen_p1.y.is_finite() {
                                                        clipped_painter_b.circle_filled(screen_p1, 3.0, egui::Color32::from_rgba_unmultiplied(220, 40, 40, 220));
                                                    }
                                                    // Log details to stderr to aid debugging (captured in terminal)
                                                    // More detailed debug: include display/center/dx/dy and viewport zoom/pan
                                                    let vp_debug = vp_b;
                                                    // compute display and center the same way pixel_to_screen does
                                                    let physical_size = vp_debug.displayed_physical_size.unwrap_or_else(|| egui::vec2(w_b as f32, h_b as f32));
                                                    let display = if vp_debug.view_mode == ViewMode::Mpr && !vp_debug.scale_by_physical {
                                                        let fit = (cell_rect.width() / (w_b as f32)).min(cell_rect.height() / (h_b as f32));
                                                        egui::vec2((w_b as f32) * fit * vp_debug.zoom, (h_b as f32) * fit * vp_debug.zoom)
                                                    } else {
                                                        let fit = (cell_rect.width() / physical_size.x).min(cell_rect.height() / physical_size.y);
                                                        egui::vec2(physical_size.x * fit * vp_debug.zoom, physical_size.y * fit * vp_debug.zoom)
                                                    };
                                                    let center = cell_rect.center() + vp_debug.pan;
                                                    let dx0 = (u0 - (w_b as f32) * 0.5) * (display.x / (w_b as f32));
                                                    let dy0 = (v0 - (h_b as f32) * 0.5) * (display.y / (h_b as f32));
                                                    let dx1 = (u1 - (w_b as f32) * 0.5) * (display.x / (w_b as f32));
                                                    let dy1 = (v1 - (h_b as f32) * 0.5) * (display.y / (h_b as f32));
                                                    // projection debug log removed
                                                    clipped_painter_b.line_segment([screen_p0, screen_p1], egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(200, 40, 40, 220)));
                                                    // Also compute and draw the corresponding segment in viewport A so both viewports show cross-refs
                                                    // compute t-range for A (similar logic as for B)
                                                    let a_u_a = dot(sub(p0, orig_a), col_a) / col_sp_a;
                                                    let b_u_a = dot(dir, col_a) / col_sp_a;
                                                    let a_v_a = dot(sub(p0, orig_a), row_a) / row_sp_a;
                                                    let b_v_a = dot(dir, row_a) / row_sp_a;

                                                    let mut t_min_a = f32::NEG_INFINITY;
                                                    let mut t_max_a = f32::INFINITY;

                                                    let ua0 = 0.0_f32;
                                                    let ua1 = (w_a as f32) - 1.0;
                                                    if b_u_a.abs() < 1e-6 {
                                                        if a_u_a < ua0 || a_u_a > ua1 {
                                                            // no intersection for A
                                                            t_min_a = 1.0;
                                                            t_max_a = 0.0;
                                                        }
                                                    } else {
                                                        let t1 = (ua0 - a_u_a) / b_u_a;
                                                        let t2 = (ua1 - a_u_a) / b_u_a;
                                                        let (ta, tb) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
                                                        t_min_a = t_min_a.max(ta);
                                                        t_max_a = t_max_a.min(tb);
                                                    }

                                                    let va0 = 0.0_f32;
                                                    let va1 = (h_a as f32) - 1.0;
                                                    if b_v_a.abs() < 1e-6 {
                                                        if a_v_a < va0 || a_v_a > va1 {
                                                            t_min_a = 1.0;
                                                            t_max_a = 0.0;
                                                        }
                                                    } else {
                                                        let t1 = (va0 - a_v_a) / b_v_a;
                                                        let t2 = (va1 - a_v_a) / b_v_a;
                                                        let (ta, tb) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
                                                        t_min_a = t_min_a.max(ta);
                                                        t_max_a = t_max_a.min(tb);
                                                    }

                                                    if t_min_a <= t_max_a && t_min_a.is_finite() && t_max_a.is_finite() {
                                                        let p_start_a = add(p0, scale(dir, t_min_a));
                                                        let p_end_a = add(p0, scale(dir, t_max_a));
                                                        let (ua0p, va0p) = self.project_point_to_pixel(orig_a, col_a, row_a, col_sp_a, row_sp_a, p_start_a);
                                                        let (ua1p, va1p) = self.project_point_to_pixel(orig_a, col_a, row_a, col_sp_a, row_sp_a, p_end_a);
                                                        let cell_rect_a = *rect_a;
                                                        let vp_a = &self.viewports[*ia];
                                                        let screen_pa0 = self.pixel_to_screen(ua0p, va0p, w_a as f32, h_a as f32, cell_rect_a, vp_a);
                                                        let screen_pa1 = self.pixel_to_screen(ua1p, va1p, w_a as f32, h_a as f32, cell_rect_a, vp_a);
                                                        let clipped_painter_a = painter.with_clip_rect(cell_rect_a);
                                                        if screen_pa0.x.is_finite() && screen_pa0.y.is_finite() {
                                                            clipped_painter_a.circle_filled(screen_pa0, 3.0, egui::Color32::from_rgba_unmultiplied(220, 40, 40, 220));
                                                        }
                                                        if screen_pa1.x.is_finite() && screen_pa1.y.is_finite() {
                                                            clipped_painter_a.circle_filled(screen_pa1, 3.0, egui::Color32::from_rgba_unmultiplied(220, 40, 40, 220));
                                                        }
                                                        clipped_painter_a.line_segment([screen_pa0, screen_pa1], egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(200, 40, 40, 220)));
                                                    }
                                                    // (debug text removed to avoid UI clutter)
                                                } else {
                                                    // intersection produced no visible segment on target; fall back to anchor-to-anchor line for visibility
                                                }
                                            }
                                        }
                                        // Fallback removed: do not draw anchor-to-anchor line to avoid duplicate UI elements
                                    }
                                }
                            }
                        }
                    }
                }

                if self.viewports.len() <= 1 {
                if active_tex.is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            if self.vp().view_mode == ViewMode::Mpr {
                                ui.label(
                                    self.active_mpr_error()
                                        .unwrap_or("MPR is not available for the selected series"),
                                );
                            } else {
                                ui.label("Open DICOM files/folders or drag and drop them here");
                            }

                            // Recent folders (clickable)
                            if !self.recent_folders.is_empty() {
                                ui.add_space(8.0);
                                ui.label("Recent:");
                                for p in &self.recent_folders {
                                    let display = p.display().to_string();
                                    if ui.add(egui::Button::new(display)).clicked() {
                                        self.pending_load = Some((vec![p.clone()], true));
                                    }
                                }
                            }
                        });
                    });
                    return;
                }

                if let Some(texture) = active_tex {
                let rect = ui.available_rect_before_wrap();
                let tex_size = texture.size();
                let img_w = tex_size[0] as f32;
                let img_h = tex_size[1] as f32;
                    let physical_size = self.vp().displayed_physical_size.unwrap_or_else(|| egui::vec2(img_w, img_h));

                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // Check button states for multi-button combinations
                let (left_down, right_down, middle_down, side1_down) = ui.ctx().input(|i| {
                    (
                        i.pointer.button_down(egui::PointerButton::Primary),
                        i.pointer.button_down(egui::PointerButton::Secondary),
                        i.pointer.button_down(egui::PointerButton::Middle),
                        i.pointer.button_down(egui::PointerButton::Extra1),
                    )
                });

                // Scroll wheel behavior:
                // - Stack mode (multiple images): scroll navigates slices.
                // - Single-image mode: scroll zooms.
                let scroll_delta = ui.ctx().input(|i| i.smooth_scroll_delta.y);

                if !side1_down {
                    self.vp_mut().rotation_drag_last_pos = None;
                }

                if !response.hovered() {
                    self.wheel_slice_accumulator = 0.0;
                    self.wheel_owner = None;
                }

                if response.hovered() && scroll_delta != 0.0 {
                    if !self.wheel_step_slices(0, scroll_delta, ui.ctx()) {
                        // Scroll to zoom (per-viewport), anchored at the cursor
                        if self.current_view_slice_len() <= 1 {
                            let new_zoom = (self.vp().zoom
                                * (scroll_delta * 0.004).exp())
                            .clamp(0.05, 20.0);
                            if let Some(anchor) = response.hover_pos() {
                                self.zoom_towards(new_zoom, anchor, rect.center());
                            } else {
                                self.vp_mut().zoom = new_zoom;
                            }
                        }
                    }
                }

                // Both left and right buttons → zoom (anchor locked at gesture start)
                    if response.hovered() && left_down && right_down {
                    let center = rect.center();
                    let anchor = match self.vp().zoom_drag_anchor {
                        Some(a) => a,
                        None => {
                            let a = response.hover_pos().unwrap_or(center);
                            let z = self.vp().zoom;
                            self.vp_mut().zoom_drag_anchor = Some(a);
                            self.vp_mut().zoom_drag_start_zoom = z;
                            a
                        }
                    };
                    // Zoom based on total vertical displacement from the anchor
                    // (downward drag zooms in, upward zooms out).
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let factor = ((pointer.y - anchor.y) * 0.007).exp();
                        let new_zoom =
                            (self.vp().zoom_drag_start_zoom * factor).clamp(0.05, 20.0);
                        self.zoom_towards(new_zoom, anchor, center);
                    }
                }
                // Middle mouse button → scroll through slices (if multiple images)
                else if response.hovered() && middle_down && self.current_view_slice_len() > 1 {
                    let delta = response.drag_delta();
                    let current = match self.vp().view_mode {
                        ViewMode::Stack => self.vp().current_stack_slice,
                        ViewMode::Mpr => self.current_mpr_slice(),
                    };
                    // Upward drag → previous slice, downward → next slice
                    if delta.y > 2.0 && current > 0 {
                        match self.vp().view_mode {
                            ViewMode::Stack => self.vp_mut().current_stack_slice -= 1,
                            ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                        }
                        self.vp_mut().wl_dirty = true;
                    } else if delta.y < -2.0 && current < self.current_view_slice_len() - 1 {
                        match self.vp().view_mode {
                            ViewMode::Stack => self.vp_mut().current_stack_slice += 1,
                            ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                        }
                        self.vp_mut().wl_dirty = true;
                    }
                }
                // Side mouse button drag (Mouse4) -> rotate image (per-viewport)
                else if response.hovered() && side1_down {
                    let image_center = rect.center() + self.vp().pan;
                    let pointer_pos = ui.ctx().input(|i| i.pointer.interact_pos());
                    if let Some(current_pos) = pointer_pos {
                        if let Some(last_pos) = self.vp().rotation_drag_last_pos {
                            let last_vec = last_pos - image_center;
                            let current_vec = current_pos - image_center;
                            let last_len2 = last_vec.length_sq();
                            let current_len2 = current_vec.length_sq();

                            // Ignore jitter when pointer is too close to the center pivot.
                            if last_len2 > 9.0 && current_len2 > 9.0 {
                                let last_angle = last_vec.y.atan2(last_vec.x);
                                let current_angle = current_vec.y.atan2(current_vec.x);
                                let mut delta_angle = current_angle - last_angle;
                                while delta_angle > std::f32::consts::PI {
                                    delta_angle -= 2.0 * std::f32::consts::PI;
                                }
                                while delta_angle < -std::f32::consts::PI {
                                    delta_angle += 2.0 * std::f32::consts::PI;
                                }

                                self.vp_mut().rotation_degrees =
                                    (self.vp().rotation_degrees + delta_angle.to_degrees())
                                        .rem_euclid(360.0);
                            }
                        }
                        self.vp_mut().rotation_drag_last_pos = Some(current_pos);
                    } else {
                        self.vp_mut().rotation_drag_last_pos = None;
                    }
                }
                // Left-button drag → pan (only if right button is not pressed)
                else if response.dragged_by(egui::PointerButton::Primary) && !right_down {
                    self.vp_mut().pan += response.drag_delta();
                }
                // Right-button drag → window / level (only if left button is not pressed)
                // Horizontal drag adjusts Window Width; vertical drag adjusts Window Centre.
                // Only meaningful for 16-bit grayscale; ignored otherwise.
                else if response.dragged_by(egui::PointerButton::Secondary) && !left_down {
                    let windowing_enabled = match self.vp().view_mode {
                        ViewMode::Stack => self
                            .active_image()
                            .map(|image| image.raw_image.is_grayscale16())
                            .unwrap_or(false),
                        ViewMode::Mpr => self.active_mpr_volume().is_some(),
                    };
                    if windowing_enabled {
                        let delta = response.drag_delta();
                        let ww_scale = 2.0_f32;
                        let wc_scale = 2.0_f32;
                        self.vp_mut().window_width = (self.vp().window_width + delta.x * ww_scale).max(1.0);
                        self.vp_mut().window_center += -delta.y * wc_scale;
                        self.vp_mut().wl_dirty = true;
                    }
                }

                // Double-click → reset view (per-viewport)
                if response.double_clicked() {
                    self.vp_mut().zoom = 1.0;
                    self.vp_mut().rotation_degrees = 0.0;
                    self.vp_mut().rotation_drag_last_pos = None;
                    self.vp_mut().pan = Vec2::ZERO;
                    self.apply_default_window_for_current_series();
                    self.vp_mut().wl_dirty = true;
                }

                let center = rect.center() + self.vp().pan;
                // Recompute the display size from the current zoom: the zoom/pan
                // interactions above run after `rect`/`physical_size` are known, and
                // must be reflected in the same frame as the pan change. Otherwise
                // the image renders at the old zoom with the new pan for one frame,
                // which makes zooming jitter.
                let display = if self.vp().view_mode == ViewMode::Mpr && !self.vp().scale_by_physical {
                    let fit = (rect.width() / img_w).min(rect.height() / img_h);
                    egui::vec2(img_w * fit * self.vp().zoom, img_h * fit * self.vp().zoom)
                } else {
                    let fit = (rect.width() / physical_size.x).min(rect.height() / physical_size.y);
                    egui::vec2(physical_size.x * fit * self.vp().zoom, physical_size.y * fit * self.vp().zoom)
                };
                let angle_rad = self.vp().rotation_degrees.to_radians();
                let painter = ui.painter();
                let clipped = painter.with_clip_rect(rect);
                paint_rotated_texture(&clipped, texture.id(), center, display, angle_rad);
                // Record the image rect for per-viewport overlay controls.
                self.viewport_image_rects = vec![Some(egui::Rect::from_center_size(center, display))];

                // Per-viewport controls drawn on top of this viewport.
                self.viewport_controls_overlay(ui, 0, rect);

                // Draw a vertical scroll indicator on the right side of the image panel
                let total_slices = self.current_view_slice_len();
                if total_slices > 1 {
                    let current_idx = match self.vp().view_mode {
                        ViewMode::Stack => self.vp().current_stack_slice,
                        ViewMode::Mpr => self.current_mpr_slice(),
                    } as usize;

                    let track_width = 12.0_f32;
                    let padding = 8.0_f32;
                    let track_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.right() - padding - track_width, rect.top() + padding),
                        egui::vec2(track_width, rect.height() - padding * 2.0),
                    );

                    // Draw track background
                    ui.painter().rect_filled(
                        track_rect,
                        track_width * 0.5,
                        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 120),
                    );

                    // Compute thumb position and size (proportional)
                    let thumb_h = (track_rect.height() / (total_slices as f32)).max(6.0);
                    let available = track_rect.height() - thumb_h;
                    let t = if total_slices > 1 {
                        (current_idx as f32) / ((total_slices - 1) as f32)
                    } else {
                        0.0
                    };
                    let thumb_y = track_rect.top() + t * available;
                    let thumb_rect = egui::Rect::from_min_size(
                        egui::pos2(track_rect.left(), thumb_y),
                        egui::vec2(track_rect.width(), thumb_h),
                    );

                    // Draw thumb
                    ui.painter().rect_filled(
                        thumb_rect,
                        6.0,
                        egui::Color32::from_rgb(200, 200, 200),
                    );

                    // Draw thin shadow/border by overlaying a semi-transparent dark rect slightly inset
                    ui.painter().rect_filled(
                        thumb_rect.shrink(0.5),
                        6.0,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 80),
                    );

                    // Click / drag anywhere on the track to navigate slices.
                    let scroll_resp = ui.interact(
                        track_rect,
                        egui::Id::new("slice_scroll_track_single"),
                        egui::Sense::click_and_drag(),
                    );
                    if scroll_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    }
                    if scroll_resp.dragged() || scroll_resp.clicked() {
                        if let Some(pointer) = scroll_resp.interact_pointer_pos() {
                            let t = ((pointer.y - track_rect.top()) / track_rect.height())
                                .clamp(0.0, 1.0);
                            let new_idx = (t * (total_slices - 1) as f32).round() as usize;
                            let changed = match self.vp().view_mode {
                                ViewMode::Stack => {
                                    if self.vp().current_stack_slice != new_idx {
                                        self.vp_mut().current_stack_slice = new_idx;
                                        true
                                    } else {
                                        false
                                    }
                                }
                                ViewMode::Mpr => {
                                    if self.current_mpr_slice() != new_idx {
                                        self.set_current_mpr_slice(new_idx);
                                        true
                                    } else {
                                        false
                                    }
                                }
                            };
                            if changed {
                                self.vp_mut().wl_dirty = true;
                            }
                        }
                    }

                    // Draw slice count text above the thumb
                    let label = format!("{}/{}", current_idx + 1, total_slices);
                    let text_pos = egui::pos2(track_rect.left() - 6.0, thumb_rect.center().y - 8.0);
                    ui.painter().text(
                        text_pos,
                        egui::Align2::RIGHT_CENTER,
                        label,
                        egui::FontId::proportional(12.0),
                        egui::Color32::WHITE,
                    );
                }
                }
            }
        });
        // ── Loading progress modal ────────────────────────────────────────────
        if let Some(state) = &self.loading {
            let total = state.total;
            let received = state.received;
            let filename = state.current_filename.clone();

            // Dim the background
            let screen = ui.ctx().viewport_rect();
            egui::Area::new(egui::Id::new("loading_overlay"))
                .fixed_pos(screen.min)
                .order(egui::Order::Background)
                .show(ui.ctx(), |ui| {
                    ui.painter().rect_filled(
                        screen,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160),
                    );
                });

            egui::Window::new("Loading")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .resizable(false)
                .collapsible(false)
                .title_bar(false)
                .fixed_size([300.0, 90.0])
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        ui.label(format!("Loading {}/{}", received, total));
                        ui.add_space(4.0);
                        ui.add(
                            egui::ProgressBar::new(received as f32 / total as f32)
                                .desired_width(260.0),
                        );
                        ui.add_space(4.0);
                        if !filename.is_empty() {
                            ui.label(
                                egui::RichText::new(&filename)
                                    .small()
                                    .color(egui::Color32::GRAY),
                            );
                        }
                        ui.add_space(8.0);
                    });
                });
        }

        // Propagate this frame's navigation to any synced viewports.
        // Propagate this frame's navigation to any synced viewports.
        self.autosync_active(ui.ctx());

        // Keep the removal preview live **only while the Align panel is open**,
        // so opening a series does no extra work until the controls are shown.
        if self.deface_preview_on
            && self.deface_panel_open
            && self.deface_preview.is_none()
            && !self.series_groups.is_empty()
            && self.loading.is_none()
        {
            self.refresh_deface_preview(ui.ctx());
        }
    }
}

/// Load every DICOM file in `dir` as one volume and render `plane`
/// (`axial`/`coronal`/`sagittal`) at `index` (default: middle) as ASCII.
///
/// Intended for manual orientation checks and the `mpr_preview` example.
pub fn mpr_preview_from_dir(
    dir: &std::path::Path,
    plane: &str,
    index: Option<usize>,
) -> Result<String, String> {
    let files = collect_dicom_files_recursively(vec![dir.to_path_buf()]);
    let mut images = Vec::new();
    for path in &files {
        if let Ok(image) = decode_single_file(path) {
            images.push(image);
        }
    }
    if images.len() < 2 {
        return Err(format!("need >= 2 slices, found {}", images.len()));
    }
    let indices: Vec<usize> = (0..images.len()).collect();
    let volume = MprVolume::from_images(&images, &indices)?;
    let plane = match plane.to_ascii_lowercase().as_str() {
        "axial" | "ax" => MprPlane::Axial,
        "coronal" | "cor" => MprPlane::Coronal,
        "sagittal" | "sag" => MprPlane::Sagittal,
        other => return Err(format!("unknown plane '{other}'")),
    };
    let len = volume.plane_len(plane);
    let idx = index.unwrap_or(len / 2).min(len.saturating_sub(1));
    let (w, h) = volume.plane_dimensions(plane);
    let pixels = volume.extract_plane(plane, idx);
    let (wc, ww) = volume.default_wc_ww;

    // Downsample to a readable ASCII grid using the volume's own window/level.
    let cols = 72usize;
    let rows = 36usize;
    let sx = (w as f64 / cols as f64).max(1.0);
    let sy = (h as f64 / rows as f64).max(1.0);
    let ramp = b" .:-=+*#%@";
    let mut out = format!(
        "{} plane {}/{}  ({}x{} px)  top=row0\n",
        plane.label(),
        idx + 1,
        len,
        w,
        h
    );
    for r in 0..rows {
        let y = ((r as f64 + 0.5) * sy) as usize;
        let mut line = String::with_capacity(cols);
        for c in 0..cols {
            let x = ((c as f64 + 0.5) * sx) as usize;
            if x >= w || y >= h {
                line.push(' ');
                continue;
            }
            let v = pixels[y * w + x];
            let t = ((v - (wc - ww / 2.0)) / ww).clamp(0.0, 1.0);
            let ch = ramp[(t * (ramp.len() - 1) as f32) as usize] as char;
            line.push(ch);
        }
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Load every DICOM file in `dir` as one series and report the geometry used
/// for defacing plus the anterior/posterior split of the removed voxels.
///
/// Intended for diagnosing `deface_debug` orientation issues.
pub fn deface_debug_from_dir(dir: &std::path::Path) -> Result<String, String> {
    let files = collect_dicom_files_recursively(vec![dir.to_path_buf()]);
    let mut images = Vec::new();
    for path in &files {
        if let Ok(image) = decode_single_file(path) {
            images.push(image);
        }
    }
    if images.len() < 2 {
        return Err(format!("need >= 2 slices, found {}", images.len()));
    }
    let indices: Vec<usize> = (0..images.len()).collect();
    let geom = build_deface_volume(&images, &indices)?;
    let volume = &geom.volume;

    let d = [volume.dir[0], volume.dir[1], volume.dir[2]];
    let norms = [
        (d[0][0] * d[0][0] + d[0][1] * d[0][1] + d[0][2] * d[0][2]).sqrt(),
        (d[1][0] * d[1][0] + d[1][1] * d[1][1] + d[1][2] * d[1][2]).sqrt(),
        (d[2][0] * d[2][0] + d[2][1] * d[2][1] + d[2][2] * d[2][2]).sqrt(),
    ];
    let dots = [
        d[0][0] * d[1][0] + d[0][1] * d[1][1] + d[0][2] * d[1][2],
        d[0][0] * d[2][0] + d[0][1] * d[2][1] + d[0][2] * d[2][2],
        d[1][0] * d[2][0] + d[1][1] * d[2][1] + d[1][2] * d[2][2],
    ];

    let backend = GeometricBackend::new(GeometricParams::default());
    let mask = backend.compute_mask(volume)?;

    // Anterior/posterior split measured against true patient Y (anterior = -Y).
    // Also record the in-plane angle about the patient Z axis.
    let (mut ant, mut post, mut total) = (0usize, 0usize, 0usize);
    let (mut min_x, mut max_x) = (f64::MAX, f64::MIN);
    let (mut min_y, mut max_y) = (f64::MAX, f64::MIN);
    for z in 0..volume.nz() {
        for y in 0..volume.ny() {
            for x in 0..volume.nx() {
                if !mask.is_removed(x, y, z) {
                    continue;
                }
                total += 1;
                let w = volume.world(x, y, z);
                if w[1] < 0.0 {
                    ant += 1;
                } else {
                    post += 1;
                }
                min_x = min_x.min(w[0]);
                max_x = max_x.max(w[0]);
                min_y = min_y.min(w[1]);
                max_y = max_y.max(w[1]);
            }
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "series: {} slices, modality {}\n",
        images.len(),
        volume.modality
    ));
    out.push_str(&format!(
        "dims: {:?}  spacing {:?}\n",
        volume.dims, volume.spacing
    ));
    out.push_str(&format!("origin: {:?}\n", volume.origin));
    out.push_str(&format!(
        "dir (x=col, y=row, z=slice):\n  x {:?}\n  y {:?}\n  z {:?}\n",
        d[0], d[1], d[2]
    ));
    out.push_str(&format!(
        "axis norms: {:.4}, {:.4}, {:.4}\n  pairwise dots: {:.4}, {:.4}, {:.4}\n",
        norms[0], norms[1], norms[2], dots[0], dots[1], dots[2]
    ));
    out.push_str(&format!(
        "removed {} voxels: anterior(-Y) {} ({:.1}%), posterior {} ({:.1}%)\n",
        total,
        ant,
        100.0 * ant as f64 / total.max(1) as f64,
        post,
        100.0 * post as f64 / total.max(1) as f64
    ));
    out.push_str(&format!(
        "removed world bbox: X [{:.1}, {:.1}]  Y [{:.1}, {:.1}]\n",
        min_x, max_x, min_y, max_y
    ));

    // Estimated head frame (patient space).
    let threshold =
        diface_rs::geometric::choose_threshold(&volume, &diface_rs::geometric::Threshold::Auto);
    if let Some(stats) = diface_rs::geometric::head_statistics(&volume, threshold, 160) {
        out.push_str(&format!(
            "head centroid {:?}\n  anterior {:?}\n  left {:?}\n  superior {:?}\n  half {:?}\n",
            stats.centroid, stats.anterior, stats.left, stats.superior, stats.half
        ));
    }

    {
        let th = diface_rs::geometric::choose_threshold(
            &volume,
            &diface_rs::geometric::Threshold::Auto,
        );
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        let mut fg = 0usize;
        for z in 0..volume.nz() {
            for y in 0..volume.ny() {
                for x in 0..volume.nx() {
                    if volume.value(x, y, z) > th {
                        fg += 1;
                        let w = volume.world(x, y, z);
                        for a in 0..3 {
                            lo[a] = lo[a].min(w[a]);
                            hi[a] = hi[a].max(w[a]);
                        }
                    }
                }
            }
        }
        out.push_str(&format!(
            "foreground {fg} voxels, physical extent XYZ = [{:.0}, {:.0}, {:.0}] mm\n",
            hi[0] - lo[0],
            hi[1] - lo[1],
            hi[2] - lo[2]
        ));
    }

    // Compare with diface-rs's own loader/geometry on the same files.
    if let Ok((refvol, _entries)) = diface_rs::series::load_series(&files) {
        let refmask = GeometricBackend::new(GeometricParams::default()).compute_mask(&refvol)?;
        let (mut ra, mut rt) = (0usize, 0usize);
        for z in 0..refvol.nz() {
            for y in 0..refvol.ny() {
                for x in 0..refvol.nx() {
                    if refmask.is_removed(x, y, z) {
                        rt += 1;
                        if refvol.world(x, y, z)[1] < 0.0 {
                            ra += 1;
                        }
                    }
                }
            }
        }
        out.push_str(&format!(
            "diface-rs ref: dims {:?} spacing {:?} removed {} anterior {:.1}%\n",
            refvol.dims,
            refvol.spacing,
            rt,
            100.0 * ra as f64 / rt.max(1) as f64
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray16_image(
        filename: &str,
        instance_number: i32,
        patient_position: [f32; 3],
        pixel_spacing: [f32; 2],
        width: usize,
        height: usize,
        data: &[f32],
    ) -> LoadedImage {
        LoadedImage {
            raw_image: RawImage::Gray16(Gray16 {
                data: data.to_vec(),
                width,
                height,
            }),
            metadata: Vec::new(),
            filename: filename.to_string(),
            series_uid: "series-1".to_string(),
            series_label: "Series 1".to_string(),
            instance_number: Some(instance_number),
            default_wc_ww: Some((0.0, 1.0)),
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some(pixel_spacing),
            slice_thickness: Some(2.0),
            spacing_between_slices: Some(2.0),
            image_position_patient: Some(patient_position),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        }
    }

    #[test]
    fn mpr_volume_sorts_slices_and_extracts_planes() {
        let images = vec![
            gray16_image("slice-2", 2, [0.0, 0.0, 2.0], [0.5, 1.5], 3, 2, &[7.0, 8.0, 9.0, 10.0, 11.0, 12.0]),
            gray16_image("slice-1", 1, [0.0, 0.0, 0.0], [0.5, 1.5], 3, 2, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
        ];

        let volume = MprVolume::from_images(&images, &[0, 1]).expect("volume should build");

        assert_eq!(volume.depth, 2);
        assert_eq!(volume.default_wc_ww, (6.5, 11.0));
        assert!((volume.slice_spacing - 2.0).abs() < 1e-6);
        assert_eq!(
            volume.extract_plane(MprPlane::Axial, 0),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        );
        // Coronal at the lowest Y row: superior at the top, so the higher slice
        // (7..9) is row 0 and the lower slice (1..3) is row 1.
        assert_eq!(
            volume.extract_plane(MprPlane::Coronal, 0),
            vec![7.0, 8.0, 9.0, 1.0, 2.0, 3.0]
        );
        // Sagittal: superior at the top (Z reversed) and anterior on the left
        // (Y increases left->right, i.e. not reversed).
        assert_eq!(
            volume.extract_plane(MprPlane::Sagittal, 2),
            vec![9.0, 12.0, 3.0, 6.0]
        );
    }

    #[test]
    fn sagittal_puts_anterior_on_the_left() {
        // 3x3 in-plane, 3 slices. Put a bright anterior marker (low Y) and a
        // bright posterior marker (high Y) at the same slice/column, then check
        // they land on the left and right of the sagittal image respectively.
        let mut images = Vec::new();
        for z in 0..3u16 {
            // 3 cols (X), 3 rows (Y). Row 0 = anterior (min Y), row 2 = posterior.
            let mut data = vec![0.0f32; 9];
            data[0 * 3 + 1] = 100.0; // anterior marker at y=0, x=1
            data[2 * 3 + 1] = 200.0; // posterior marker at y=2, x=1
            let mut img = gray16_image(
                &format!("s{z}"),
                z as i32 + 1,
                [0.0, 0.0, z as f32 * 2.0],
                [1.0, 1.0],
                3,
                3,
                &data,
            );
            // Identity orientation: x = patient left(+X), y = patient posterior(+Y).
            img.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
            images.push(img);
        }
        let volume = MprVolume::from_images(&images, &[0, 1, 2]).expect("volume");
        // Sagittal plane at the x=1 column.
        let width = volume.axis_samples(PatientAxis::Y);
        let height = volume.axis_samples(PatientAxis::Z);
        let sag = volume.extract_plane(MprPlane::Sagittal, 1);
        assert_eq!(sag.len(), width * height);

        // Find where each marker appears.
        let mut ant_col = None;
        let mut post_col = None;
        for row in 0..height {
            for col in 0..width {
                match sag[row * width + col] {
                    v if (v - 100.0).abs() < 1e-3 => ant_col = Some(col),
                    v if (v - 200.0).abs() < 1e-3 => post_col = Some(col),
                    _ => {}
                }
            }
        }
        let ant_col = ant_col.expect("anterior marker present");
        let post_col = post_col.expect("posterior marker present");
        assert!(
            ant_col < post_col,
            "anterior must be left of posterior (ant_col={ant_col}, post_col={post_col})"
        );
    }

    #[test]
    fn mpr_planes_follow_patient_orientation_for_coronal_acquisition() {
        let mut images = vec![
            gray16_image("slice-2", 2, [0.0, -2.0, 0.0], [0.5, 1.5], 3, 2, &[7.0, 8.0, 9.0, 10.0, 11.0, 12.0]),
            gray16_image("slice-1", 1, [0.0, 0.0, 0.0], [0.5, 1.5], 3, 2, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
        ];
        for image in &mut images {
            image.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        }

        let volume = MprVolume::from_images(&images, &[0, 1]).expect("volume should build");

        assert_eq!(volume.plane_dimensions(MprPlane::Axial), (3, 2));
        assert_eq!(
            volume.extract_plane(MprPlane::Axial, 1),
            vec![10.0, 11.0, 12.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(volume.plane_len(MprPlane::Coronal), 2);
    }

    #[test]
    fn mpr_resamples_oblique_series_in_patient_space() {
        let mut images = vec![
            gray16_image("slice-2", 2, [0.0, 0.0, 2.0], [1.0, 1.0], 2, 2, &[5.0, 6.0, 7.0, 8.0]),
            gray16_image("slice-1", 1, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &[1.0, 2.0, 3.0, 4.0]),
        ];
        let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
        for image in &mut images {
            image.image_orientation_patient = Some([inv_sqrt2, inv_sqrt2, 0.0, 0.0, 0.0, 1.0]);
        }
        images[0].image_position_patient = Some([-inv_sqrt2 * 2.0, inv_sqrt2 * 2.0, 0.0]);
        images[1].image_position_patient = Some([0.0, 0.0, 0.0]);

        let volume = MprVolume::from_images(&images, &[0, 1]).expect("oblique volume should build");

        assert!(volume.plane_len(MprPlane::Sagittal) >= 2);
        assert!(volume.plane_dimensions(MprPlane::Axial).0 >= 2);
        assert!(volume.plane_dimensions(MprPlane::Axial).1 >= 2);
        let axial = volume.extract_plane(MprPlane::Axial, 0);
        assert!(axial.iter().any(|value| (*value - 1.0).abs() < 1e-3));
        assert!(axial.iter().copied().fold(f32::MIN, f32::max) > 1.5);
    }

    #[test]
    fn mpr_handles_gantry_tilt_shear() {
        // Tilted rows (IOP row vector leans into -Y) with slices advancing along
        // +Z: a sheared stack. A feature placed at the *same patient point* on
        // every slice must sample as the same value regardless of tilt.
        let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
        let row_dir = [0.0, inv_sqrt2, -inv_sqrt2];
        let col_dir = [1.0, 0.0, 0.0];
        let iop = [col_dir[0], col_dir[1], col_dir[2], row_dir[0], row_dir[1], row_dir[2]];

        // Two 4x4 slices, spacing 1mm. Each slice gets a bright voxel whose
        // in-plane pixel position compensates for the tilt so that all bright
        // voxels share one patient coordinate.
        let mut images = Vec::new();
        let base = [0.0f32, 0.0, 0.0];
        for s in 0..2 {
            let mut data = vec![0.0f32; 16];
            // bright at (col=2, row=0) on every slice
            data[0 * 4 + 2] = 100.0;
            let mut img = gray16_image(
                &format!("tilt-{s}"),
                s as i32,
                [base[0], base[1], base[2] + s as f32 * 2.0],
                [1.0, 1.0],
                4,
                4,
                &data,
            );
            img.image_orientation_patient = Some(iop);
            images.push(img);
        }

        let volume = MprVolume::from_images(&images, &[0, 1]).expect("tilted volume should build");

        // Slice s has its bright voxel at patient point
        //   pos(s) + 2*col_dir  (pixel (col=2,row=0), spacing 1mm).
        // Verify the sampler recovers it exactly from patient space, which is
        // only possible if the per-slice tilt is honoured.
        for (s, image) in images.iter().enumerate() {
            let pos = image.image_position_patient.unwrap();
            let patient = [
                pos[0] + 2.0 * col_dir[0],
                pos[1] + 2.0 * col_dir[1],
                pos[2] + 2.0 * col_dir[2],
            ];
            let value = volume.sample_trilinear(patient);
            assert!(
                (value - 100.0).abs() < 1e-3,
                "slice {s}: expected 100 at its bright voxel's patient point, got {value}"
            );
        }

        // A 45-degree tilt must not collapse the through-plane (Z) step to the
        // in-plane pixel size. The projected source-voxel extent along Z is
        // 1*|sin45| + 2*|cos45| ~= 2.12mm, not ~0.71mm.
        let z_spacing = volume.axis_spacing(PatientAxis::Z);
        assert!(
            z_spacing > 1.5,
            "through-plane spacing should stay coarse on a tilted stack, got {z_spacing}"
        );

        // Axial (through-plane = Z) should therefore be a small stack, not one
        // sample per fine in-plane step.
        let axial_len = volume.plane_len(MprPlane::Axial);
        assert!(
            axial_len <= 4,
            "tilted CT should not oversample the axial stack, got {axial_len}"
        );
    }

    #[test]
    fn mpr_volume_builds_from_gray8_series() {
        let mk = |name: &str, inst: i32, z: f32| LoadedImage {
            raw_image: RawImage::Gray8 {
                data: vec![1, 2, 3, 4, 5, 6],
                width: 3,
                height: 2,
            },
            metadata: Vec::new(),
            filename: name.to_string(),
            series_uid: "s".to_string(),
            series_label: "S".to_string(),
            instance_number: Some(inst),
            default_wc_ww: Some((3.0, 6.0)),
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: Some(2.0),
            spacing_between_slices: Some(2.0),
            image_position_patient: Some([0.0, 0.0, z]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        };
        let images = vec![mk("a", 1, 0.0), mk("b", 2, 2.0)];
        let volume = MprVolume::from_images(&images, &[0, 1]).expect("gray8 volume builds");
        assert_eq!(volume.depth, 2);
        assert_eq!(volume.width, 3);
        assert_eq!(volume.height, 2);
    }

    #[test]
    fn mpr_volume_rejects_rgb_series() {
        let images = vec![LoadedImage {
            raw_image: RawImage::Rgb8 {
                data: vec![0, 0, 0, 255, 255, 255],
                width: 1,
                height: 2,
            },
            metadata: Vec::new(),
            filename: "rgb".to_string(),
            series_uid: "series-1".to_string(),
            series_label: "Series 1".to_string(),
            instance_number: Some(1),
            default_wc_ww: None,
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: Some(1.0),
            spacing_between_slices: Some(1.0),
            image_position_patient: Some([0.0, 0.0, 0.0]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        }];

        let err = MprVolume::from_images(&images, &[0]).expect_err("rgb series should fail");
        assert!(err.contains("at least 2 slices") || err.contains("grayscale"));
    }

    #[test]
    fn presets_follow_active_viewport_modality() {
        let mut ct = gray16_image("ct", 1, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &[0.0, 1.0, 2.0, 3.0]);
        ct.metadata = vec![("Modality".to_string(), "CT".to_string())];
        let mut mr = gray16_image("mr", 1, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &[0.0, 1.0, 2.0, 3.0]);
        mr.metadata = vec![("Modality".to_string(), "MR".to_string())];
        mr.series_uid = "series-2".to_string();
        mr.series_label = "Series 2".to_string();

        let mut app = DicomViewApp::new(None);
        app.images = vec![ct, mr];
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.viewports[0].current_series = 0;
        app.viewports[1].current_series = 1;

        app.active_viewport = 0;
        assert_eq!(app.active_modality().as_deref(), Some("CT"));
        app.active_viewport = 1;
        assert_eq!(app.active_modality().as_deref(), Some("MR"));
    }

    /// Build a synthetic axial head series with a protruding nose.
    fn synthetic_head_series() -> Vec<LoadedImage> {
        let width = 32usize;
        let height = 32usize;
        let nz = 20usize;
        let spacing = 2.0f32;
        let half = width as f32 * spacing / 2.0;
        (0..nz)
            .map(|z| {
                let z_world = z as f32 * spacing - (nz as f32 * spacing) / 2.0;
                let mut data = vec![-1000.0f32; width * height];
                for y in 0..height {
                    for x in 0..width {
                        let wx = -half + x as f32 * spacing;
                        let wy = -half + y as f32 * spacing;
                        let e = (wx / 14.0).powi(2)
                            + (wy / 14.0).powi(2)
                            + (z_world / 18.0).powi(2);
                        let mut v = -1000.0f32;
                        if e <= 1.0 {
                            v = 40.0;
                        }
                        if wy <= -14.0 && wy >= -18.0 && wx.abs() <= 2.0 && z_world.abs() <= 3.0 {
                            v = 40.0;
                        }
                        data[y * width + x] = v;
                    }
                }
                gray16_image(
                    &format!("slice-{z}"),
                    z as i32 + 1,
                    [-half, -half, z_world],
                    [spacing, spacing],
                    width,
                    height,
                    &data,
                )
            })
            .collect()
    }

    #[test]
    fn deface_active_series_adds_comparison_viewport() {
        let mut app = DicomViewApp::new(None);
        app.images = synthetic_head_series();
        app.rebuild_series_groups();
        assert_eq!(app.images.len(), 20);
        assert_eq!(app.series_groups.len(), 1);

        let ctx = egui::Context::default();
        app.deface_active_series(&ctx);

        // A second, distinct series was added with a full set of slices.
        assert_eq!(app.images.len(), 40, "defaced copies should be added");
        assert_eq!(app.series_groups.len(), 2, "defaced series should be grouped");
        let def_idx = app
            .series_groups
            .iter()
            .position(|g| g.uid.ends_with("-DEFACED"))
            .expect("defaced series present");
        let def_group = &app.series_groups[def_idx];
        assert_eq!(def_group.image_indices.len(), 20);
        assert!(def_group.label.contains("[defaced]"));

        // Two viewports, original and defaced, selected for comparison.
        assert!(app.viewports.len() >= 2);
        let selected: Vec<usize> = app.viewports.iter().map(|vp| vp.current_series).collect();
        assert!(selected.contains(&0), "original series should be shown");
        assert!(
            selected.contains(&def_idx),
            "defaced series should be shown"
        );

        // Nose voxel (was 40) is blanked; deep tissue (was 40) is preserved.
        let width = 32usize;
        let mid = def_group.image_indices[10]; // z_world == 0 for nz=20, spacing=2
        let RawImage::Gray16(g) = &app.images[mid].raw_image else {
            panic!("expected grayscale image");
        };
        let nose = g.data[8 * width + 16];
        let deep = g.data[18 * width + 16];
        assert_eq!(nose, -1000.0, "nose should be blanked");
        assert_eq!(deep, 40.0, "deep tissue should be preserved");

        assert!(app.deface_message.is_some());

        // Manual params change the result: yaw 90 degrees rotates the removal.
        let auto_data = {
            let idx = app.series_groups[def_idx].image_indices[10];
            match &app.images[idx].raw_image {
                RawImage::Gray16(g) => g.data.clone(),
                _ => panic!("expected grayscale"),
            }
        };
        app.deface_params.yaw_deg = 90.0;
        app.deface_active_series(&ctx);
        let def_idx2 = app
            .series_groups
            .iter()
            .position(|g| g.uid.ends_with("-DEFACED"))
            .expect("defaced series present");
        let mid2 = app.series_groups[def_idx2].image_indices[10];
        let RawImage::Gray16(g2) = &app.images[mid2].raw_image else {
            panic!("expected grayscale image");
        };
        assert_ne!(
            auto_data, g2.data,
            "a 90-degree yaw must change the defaced slice"
        );

        // Reset back to auto and re-apply.
        app.deface_params = GeometricParams::default();

        // Clicking again refreshes the copy instead of duplicating it.
        app.deface_active_series(&ctx);
        assert_eq!(app.images.len(), 40, "repeated deface must not duplicate");
        assert_eq!(app.series_groups.len(), 2);

        // Defacing auto-links the two panes: scrolling one follows the other.
        let group = app.viewports[0].sync_group;
        assert!(group.is_some(), "deface should link the two viewports");
        assert_eq!(app.viewports[1].sync_group, group);
        app.active_viewport = 0;
        app.last_navs = app.all_nav_snapshots();
        app.viewports[0].current_stack_slice = 5;
        app.autosync_active(&ctx);
        assert_eq!(
            app.viewports[1].current_stack_slice, 5,
            "synced viewport should follow the navigated one"
        );
    }

    #[test]
    fn defacing_does_not_contaminate_source_mpr() {
        let ctx = egui::Context::default();
        let mut images = synthetic_head_series();
        for img in &mut images {
            img.study_uid = Some("study-1".to_string());
        }
        let mut app = DicomViewApp::new(None);
        app.images = images;
        app.rebuild_series_groups();

        app.deface_active_series(&ctx);
        assert_eq!(app.series_groups.len(), 2, "defaced series was added");

        // Each series gets its own MPR volume, and neither merges the other.
        let orig_idx = app
            .series_groups
            .iter()
            .position(|g| !g.uid.ends_with("-DEFACED"))
            .unwrap();
        let def_idx = app
            .series_groups
            .iter()
            .position(|g| g.uid.ends_with("-DEFACED"))
            .unwrap();
        app.refresh_mpr_volumes();
        assert_eq!(app.mpr_volumes.len(), 2);
        assert_eq!(
            app.mpr_volumes[orig_idx].as_ref().map(|v| v.depth),
            Some(20),
            "original MPR volume must not merge the defaced slices"
        );
        assert_eq!(
            app.mpr_volumes[def_idx].as_ref().map(|v| v.depth),
            Some(20),
            "defaced series gets its own MPR volume"
        );

        // The original's MPR volume must be built from un-blanked pixels, the
        // defaced one from blanked pixels.
        let orig_volume = app.mpr_volumes[orig_idx].as_ref().unwrap();
        let def_volume = app.mpr_volumes[def_idx].as_ref().unwrap();
        assert!(
            orig_volume.data.iter().any(|v| *v > -500.0 && *v < 0.0) || orig_volume.data.iter().any(|v| *v == 40.0),
            "original volume should contain tissue"
        );
        // The defaced volume retains the same bounds; the source volume must not
        // have been mutated to the fill value.
        assert_ne!(
            orig_volume.data, def_volume.data,
            "original and defaced MPR volumes should differ"
        );
    }

    #[test]
    fn defaced_series_has_its_own_mpr_volume() {
        let ctx = egui::Context::default();
        let mut images = synthetic_head_series();
        for img in &mut images {
            img.study_uid = Some("study-1".to_string());
        }
        let mut app = DicomViewApp::new(None);
        app.images = images;
        app.rebuild_series_groups();
        app.deface_active_series(&ctx);

        let def_idx = app
            .series_groups
            .iter()
            .position(|g| g.uid.ends_with("-DEFACED"))
            .unwrap();

        // Show the defaced series in one viewport and the original in another,
        // both in MPR, then verify they resolve to *different* volumes.
        let orig_idx = app
            .series_groups
            .iter()
            .position(|g| !g.uid.ends_with("-DEFACED"))
            .unwrap();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.viewports[0].current_series = orig_idx;
        app.viewports[0].view_mode = ViewMode::Mpr;
        app.viewports[1].current_series = def_idx;
        app.viewports[1].view_mode = ViewMode::Mpr;
        app.refresh_mpr_volumes();

        assert_eq!(
            app.mpr_series_for(0),
            Some(orig_idx),
            "original viewport must use the original volume"
        );
        assert_eq!(
            app.mpr_series_for(1),
            Some(def_idx),
            "defaced viewport must use the defaced volume, not the original"
        );

        // The two viewports must render different planes: render the central
        // axial plane of each and compare the intensity buffers.
        fn axial(volume: &MprVolume) -> Vec<f32> {
            volume.extract_plane(MprPlane::Axial, volume.plane_len(MprPlane::Axial) / 2)
        }
        let orig_plane = axial(&app.mpr_volumes[orig_idx].as_ref().unwrap().clone());
        let def_plane = axial(&app.mpr_volumes[def_idx].as_ref().unwrap().clone());
        assert_ne!(
            orig_plane, def_plane,
            "defaced MPR plane must differ from the original MPR plane"
        );
        // The defaced plane must actually contain blanked (fill) voxels.
        let min_raw = def_plane.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            def_plane.iter().any(|v| (*v - min_raw).abs() < 0.5),
            "defaced MPR plane should contain blanked voxels"
        );
    }

    /// Two distinct synthetic series with identical geometry.
    fn two_head_series() -> (Vec<LoadedImage>, Vec<LoadedImage>) {
        let mut a = synthetic_head_series();
        for img in &mut a {
            img.series_uid = "A".to_string();
            img.series_label = "A".to_string();
        }
        let mut b = synthetic_head_series();
        for img in &mut b {
            img.series_uid = "B".to_string();
            img.series_label = "B".to_string();
        }
        (a, b)
    }

    #[test]
    fn sync_propagates_stack_navigation_by_position() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        let (a, b) = two_head_series();
        app.images = a;
        app.images.extend(b);
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.active_viewport = 0;
        app.viewports[0].current_series =
            app.series_groups.iter().position(|g| g.uid == "A").unwrap();
        app.viewports[1].current_series =
            app.series_groups.iter().position(|g| g.uid == "B").unwrap();
        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);

        app.autosync_active(&ctx); // establish baseline
        app.viewports[0].current_stack_slice = 7;
        app.autosync_active(&ctx);

        assert_eq!(app.viewports[1].current_stack_slice, 7);
    }

    #[test]
    fn sync_propagates_mpr_navigation() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        let (a, b) = two_head_series();
        app.images = a;
        app.images.extend(b);
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.active_viewport = 0;
        app.viewports[0].current_series =
            app.series_groups.iter().position(|g| g.uid == "A").unwrap();
        app.viewports[1].current_series =
            app.series_groups.iter().position(|g| g.uid == "B").unwrap();
        app.viewports[0].view_mode = ViewMode::Mpr;
        app.viewports[1].view_mode = ViewMode::Mpr;
        app.refresh_mpr_volumes();
        assert!(app.active_mpr_volume().is_some(), "MPR volume should build");

        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);
        app.autosync_active(&ctx); // baseline
        app.viewports[0].current_axial_slice = 5;
        app.autosync_active(&ctx);

        assert_eq!(app.viewports[1].current_axial_slice, 5);
    }

    #[test]
    fn wheel_scrolling_is_damped() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        app.images = synthetic_head_series();
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;

        // Sub-threshold movement does nothing (scroll down = negative delta).
        assert!(!app.wheel_step_slices(0, -20.0, &ctx));
        assert_eq!(app.viewports[0].current_stack_slice, 0);

        // Crossing the threshold advances exactly one slice.
        assert!(app.wheel_step_slices(0, -40.0, &ctx));
        assert_eq!(app.viewports[0].current_stack_slice, 1);

        // A large flick of egui's smooth tail still advances only one slice.
        assert!(app.wheel_step_slices(0, -1000.0, &ctx));
        assert_eq!(app.viewports[0].current_stack_slice, 2);

        // Scrolling up goes back one slice.
        assert!(app.wheel_step_slices(0, 100.0, &ctx));
        assert_eq!(app.viewports[0].current_stack_slice, 1);

        // Changing the hovered viewport discards partial movement.
        assert!(!app.wheel_step_slices(0, -20.0, &ctx));
        assert!(!app.wheel_step_slices(1, -20.0, &ctx));
        assert_eq!(app.viewports[0].current_stack_slice, 1);
    }

    #[test]
    fn per_viewport_groups_isolate_sync() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        let mut images = synthetic_head_series();
        for img in &mut images {
            img.series_uid = "A".to_string();
            img.series_label = "A".to_string();
        }
        app.images = images;
        for uid in ["B", "C"] {
            let mut s = synthetic_head_series();
            for img in &mut s {
                img.series_uid = uid.to_string();
                img.series_label = uid.to_string();
            }
            app.images.extend(s);
        }
        app.rebuild_series_groups();
        app.viewports = vec![
            ViewportState::default(),
            ViewportState::default(),
            ViewportState::default(),
        ];
        for (i, uid) in ["A", "B", "C"].iter().enumerate() {
            app.viewports[i].current_series =
                app.series_groups.iter().position(|g| g.uid == *uid).unwrap();
        }
        app.active_viewport = 0;
        // A and B share group 0; C is independent.
        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);
        app.viewports[2].sync_group = None;

        app.autosync_active(&ctx); // baseline
        app.viewports[0].current_stack_slice = 4;
        app.autosync_active(&ctx);

        assert_eq!(app.viewports[1].current_stack_slice, 4, "linked member follows");
        assert_eq!(
            app.viewports[2].current_stack_slice, 0,
            "independent viewport must not move"
        );
    }

    #[test]
    fn sync_disabled_leaves_other_viewports_alone() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        let (a, b) = two_head_series();
        app.images = a;
        app.images.extend(b);
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.active_viewport = 0;
        app.viewports[0].current_series =
            app.series_groups.iter().position(|g| g.uid == "A").unwrap();
        app.viewports[1].current_series =
            app.series_groups.iter().position(|g| g.uid == "B").unwrap();
        // No sync groups assigned: viewports are independent.

        app.autosync_active(&ctx);
        app.viewports[0].current_stack_slice = 7;
        app.autosync_active(&ctx);

        assert_eq!(app.viewports[1].current_stack_slice, 0);
    }

    #[test]
    fn build_deface_volume_rejects_rgb() {
        let rgb = LoadedImage {
            raw_image: RawImage::Rgb8 {
                data: vec![0, 0, 0, 255, 255, 255, 0, 0, 0],
                width: 3,
                height: 1,
            },
            metadata: Vec::new(),
            filename: "rgb".to_string(),
            series_uid: "series-1".to_string(),
            series_label: "Series 1".to_string(),
            instance_number: Some(1),
            default_wc_ww: None,
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: Some(1.0),
            spacing_between_slices: Some(1.0),
            image_position_patient: Some([0.0, 0.0, 0.0]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        };
        let images = vec![rgb.clone(), rgb];
        let err = match build_deface_volume(&images, &[0, 1]) {
            Ok(_) => panic!("rgb series should be rejected"),
            Err(e) => e,
        };
        assert!(err.contains("grayscale"), "unexpected error: {err}");
    }

    #[test]
    fn build_deface_volume_keeps_the_largest_orientation_group() {
        let data = vec![0.0f32; 4];
        // One slice with orientation A (sagittal, slice normal = -Y).
        let mut a = gray16_image("a", 1, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &data);
        a.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        // Three slices with orientation B (axial, slice normal = +Z).
        let mut b0 = gray16_image("b0", 2, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &data);
        b0.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let mut b1 = gray16_image("b1", 3, [0.0, 0.0, 1.0], [1.0, 1.0], 2, 2, &data);
        b1.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let mut b2 = gray16_image("b2", 4, [0.0, 0.0, 2.0], [1.0, 1.0], 2, 2, &data);
        b2.image_orientation_patient = Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

        let images = vec![a, b0, b1, b2];
        let geom = build_deface_volume(&images, &[0, 1, 2, 3]).expect("geometry");
        assert_eq!(
            geom.order,
            vec![1, 2, 3],
            "the largest orientation group should be selected"
        );
        assert_eq!(geom.volume.dims[2], 3);
    }

    // ── Pure helper functions ───────────────────────────────────────────────

    #[test]
    fn mpr_plane_labels() {
        assert_eq!(MprPlane::Axial.label(), "Axial");
        assert_eq!(MprPlane::Coronal.label(), "Coronal");
        assert_eq!(MprPlane::Sagittal.label(), "Sagittal");
    }

    #[test]
    fn patient_axes_and_signs_follow_convention() {
        // Axial: u=X, v=+Y, w=Z.
        assert_eq!(
            MprPlane::Axial.patient_axes(),
            (PatientAxis::X, PatientAxis::Y, PatientAxis::Z)
        );
        assert_eq!(MprPlane::Axial.u_sign(), 1.0);
        assert_eq!(MprPlane::Axial.v_sign(), 1.0);
        // Coronal: superior at the top (v=-Z), left on the left.
        assert_eq!(
            MprPlane::Coronal.patient_axes(),
            (PatientAxis::X, PatientAxis::Z, PatientAxis::Y)
        );
        assert_eq!(MprPlane::Coronal.v_sign(), -1.0);
        // Sagittal: superior top, anterior left (u=+Y posterior, unflipped).
        assert_eq!(
            MprPlane::Sagittal.patient_axes(),
            (PatientAxis::Y, PatientAxis::Z, PatientAxis::X)
        );
        assert_eq!(MprPlane::Sagittal.u_sign(), 1.0);
        assert_eq!(MprPlane::Sagittal.v_sign(), -1.0);
    }

    #[test]
    fn raw_image_dimensions_and_grayscale_detection() {
        let g8 = RawImage::Gray8 { data: vec![0; 6], width: 3, height: 2 };
        assert_eq!(g8.dimensions(), (3, 2));
        assert!(!g8.is_grayscale16());

        let g16 = RawImage::Gray16(Gray16 { data: vec![0.0; 6], width: 3, height: 2 });
        assert_eq!(g16.dimensions(), (3, 2));
        assert!(g16.is_grayscale16());

        let rgb = RawImage::Rgb8 { data: vec![0; 6], width: 2, height: 1 };
        assert_eq!(rgb.dimensions(), (2, 1));
        assert!(!rgb.is_grayscale16());
    }

    #[test]
    fn raw_image_to_rgba_produces_expected_buffers() {
        let g8 = RawImage::Gray8 { data: vec![10, 20], width: 2, height: 1 };
        let rgba = g8.to_rgba(0.0, 1.0);
        assert_eq!(rgba.len(), 8);
        assert_eq!(&rgba[0..4], &[10, 10, 10, 255]);
        assert_eq!(&rgba[4..8], &[20, 20, 20, 255]);

        let rgb = RawImage::Rgb8 { data: vec![1, 2, 3, 4, 5, 6], width: 2, height: 1 };
        let rgba = rgb.to_rgba(0.0, 1.0);
        assert_eq!(&rgba[0..4], &[1, 2, 3, 255]);
        assert_eq!(&rgba[4..8], &[4, 5, 6, 255]);

        // 16-bit uses window/level for mapping.
        let g16 = RawImage::Gray16(Gray16 { data: vec![0.0, 10.0, 20.0], width: 3, height: 1 });
        let rgba = g16.to_rgba(10.0, 20.0);
        assert_eq!(rgba.len(), 12);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn scalar_to_rgba_maps_window_level() {
        // wc=10, ww=20 -> lo=0, hi=20.
        let out = scalar_to_rgba(&[0.0, 10.0, 20.0, -5.0, 25.0], 10.0, 20.0);
        assert_eq!(out.len(), 5 * 4);
        assert_eq!(out[0], 0); // at lo
        assert_eq!(out[3], 255); // opaque
        let mid = out[4];
        assert!((100..=155).contains(&mid), "mid grey, got {mid}");
        assert_eq!(out[8], 255); // at hi, clamped
        assert_eq!(out[12], 0); // below lo, clamped
        assert_eq!(out[16], 255); // above hi, clamped
    }

    #[test]
    fn make_thumbnail_preserves_aspect_and_size() {
        let img = RawImage::Gray16(Gray16 {
            data: (0..(64 * 32)).map(|i| i as f32).collect(),
            width: 64,
            height: 32,
        });
        let (w, h, rgba) = make_thumbnail(&img, 16, 0.0, 63.0);
        assert!(w <= 16 && h <= 16, "thumbnail fits in box, got {w}x{h}");
        assert_eq!(w.max(h), 16, "longest edge equals max_dim");
        assert_eq!(rgba.len(), w * h * 4);
    }

    #[test]
    fn rotate_vec2_rotates_by_angle() {
        let v = rotate_vec2(egui::vec2(1.0, 0.0), std::f32::consts::FRAC_PI_2);
        assert!((v.x).abs() < 1e-5 && (v.y - 1.0).abs() < 1e-5, "got {v:?}");
    }

    // ── Geometry helpers ────────────────────────────────────────────────────

    #[test]
    fn intersect_planes_returns_line_and_handles_parallel() {
        // z=0 plane and y=0 plane intersect along the x axis through origin.
        let (point, dir) =
            DicomViewApp::intersect_planes([0.0, 0.0, 1.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0])
                .expect("planes intersect");
        assert!(point.iter().all(|c| c.abs() < 1e-4));
        let d = normalize(dir).unwrap();
        assert!(d[0].abs() > 0.99, "line runs along x, got {d:?}");

        // Parallel planes have no intersection.
        assert!(DicomViewApp::intersect_planes(
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        )
        .is_none());
    }

    #[test]
    fn project_point_to_pixel_maps_world_to_pixel_indices() {
        let app = DicomViewApp::new(None);
        // Origin at 0, x maps to column, y to row, spacing 2mm.
        let (u, v) = app.project_point_to_pixel(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            2.0,
            2.0,
            [4.0, 6.0, 0.0],
        );
        assert!((u - 2.0).abs() < 1e-5);
        assert!((v - 3.0).abs() < 1e-5);
    }

    #[test]
    fn pixel_to_screen_is_centered_without_pan() {
        let app = DicomViewApp::new(None);
        let vp = ViewportState::default();
        let cell = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // The image centre pixel should land at the cell centre.
        let p = app.pixel_to_screen(50.0, 50.0, 100.0, 100.0, cell, &vp);
        assert!((p.x - 50.0).abs() < 1e-3 && (p.y - 50.0).abs() < 1e-3, "got {p:?}");
        // A corner should be offset from centre.
        let corner = app.pixel_to_screen(0.0, 0.0, 100.0, 100.0, cell, &vp);
        assert!(corner.x < 50.0 && corner.y < 50.0);
    }

    // ── App accessors over a MPR-capable stack ─────────────────────────────

    fn app_with_head() -> DicomViewApp {
        let mut app = DicomViewApp::new(None);
        app.images = synthetic_head_series();
        app.rebuild_series_groups();
        app
    }

    #[test]
    fn mpr_slice_helpers_roundtrip_per_plane() {
        let mut app = app_with_head();
        for plane in [MprPlane::Axial, MprPlane::Coronal, MprPlane::Sagittal] {
            app.vp_mut().mpr_plane = plane;
            app.set_current_mpr_slice(3);
            assert_eq!(app.current_mpr_slice(), 3, "plane {plane:?}");
        }
    }

    #[test]
    fn mpr_available_and_default_window() {
        let mut app = app_with_head();
        assert!(app.mpr_available(), "head series supports MPR");
        assert!(app.active_default_wc_ww().is_some());
        app.vp_mut().view_mode = ViewMode::Mpr;
        assert!(app.active_default_wc_ww().is_some());
    }

    #[test]
    fn active_series_grayscale_and_metadata_image() {
        let mut app = app_with_head();
        assert!(app.active_series_is_grayscale16());
        assert!(app.metadata_image().is_some());

        // An RGB series is not defaceable / grayscale.
        app.images = vec![LoadedImage {
            raw_image: RawImage::Rgb8 { data: vec![0, 0, 0], width: 1, height: 1 },
            metadata: Vec::new(),
            filename: "rgb".to_string(),
            series_uid: "rgb".to_string(),
            series_label: "RGB".to_string(),
            instance_number: Some(1),
            default_wc_ww: None,
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: None,
            spacing_between_slices: None,
            image_position_patient: Some([0.0, 0.0, 0.0]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        }];
        app.rebuild_series_groups();
        assert!(!app.active_series_is_grayscale16());
    }

    #[test]
    fn current_view_slice_len_tracks_mode() {
        let mut app = app_with_head();
        app.vp_mut().view_mode = ViewMode::Stack;
        assert_eq!(app.current_view_slice_len(), app.active_series_len());
        app.vp_mut().view_mode = ViewMode::Mpr;
        assert!(app.current_view_slice_len() > 1);
    }

    #[test]
    fn sync_group_ids_increment_and_dedupe() {
        let mut app = app_with_head();
        app.viewports = vec![
            ViewportState::default(),
            ViewportState::default(),
            ViewportState::default(),
        ];
        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);
        app.viewports[2].sync_group = Some(2);
        assert_eq!(app.sync_groups(), vec![0, 2]);
        assert_eq!(app.next_sync_group(), 3);
    }

    #[test]
    fn zoom_towards_keeps_anchor_fixed() {
        let mut app = app_with_head();
        let center = egui::pos2(100.0, 100.0);
        let anchor = egui::pos2(150.0, 100.0);
        let pan_before = app.vp().pan;
        app.zoom_towards(2.0, anchor, center);
        assert!((app.vp().zoom - 2.0).abs() < 1e-4);
        // Pan changes so the anchor stays over the same image point.
        assert_ne!(app.vp().pan, pan_before);
        // Zoom is clamped.
        app.zoom_towards(1000.0, anchor, center);
        assert!(app.vp().zoom <= 20.0);
        app.zoom_towards(0.0001, anchor, center);
        assert!(app.vp().zoom >= 0.05);
    }

    #[test]
    fn viewport_plane_stack_and_mpr() {
        let mut app = app_with_head();
        let stack = app.viewport_plane(0).expect("stack plane");
        assert_eq!(stack.6, 32); // width
        assert_eq!(stack.7, 32); // height

        app.vp_mut().view_mode = ViewMode::Mpr;
        app.refresh_mpr_volumes();
        let mpr = app.viewport_plane(0).expect("mpr plane");
        assert!(mpr.6 > 0 && mpr.7 > 0);
    }

    #[test]
    fn wheel_zoom_branch_for_single_slice() {
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        // A single-image series has slice len 1, so wheel returns false (zoom).
        app.images = vec![gray16_image("only", 1, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &[0.0; 4])];
        app.rebuild_series_groups();
        assert_eq!(app.current_view_slice_len(), 1);
        assert!(!app.wheel_step_slices(0, -50.0, &ctx));
    }

    // ── File discovery helpers ──────────────────────────────────────────────

    #[test]
    fn dicom_extension_and_support_checks() {
        assert!(has_supported_dicom_extension(std::path::Path::new("a.dcm")));
        assert!(has_supported_dicom_extension(std::path::Path::new("a.DCM")));
        assert!(has_supported_dicom_extension(std::path::Path::new("a.dicom")));
        assert!(!has_supported_dicom_extension(std::path::Path::new("a.txt")));
        assert!(!has_supported_dicom_extension(std::path::Path::new("noext")));

        // Existing file with a known extension is supported.
        let tmp = tempfile::tempdir().unwrap();
        let dcm = tmp.path().join("x.dcm");
        std::fs::write(&dcm, b"not really dicom").unwrap();
        assert!(is_supported_dicom_file(&dcm));
        // A .txt file with no DICOM preamble is not.
        let txt = tmp.path().join("x.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(!is_supported_dicom_file(&txt));
    }

    #[test]
    fn has_dicom_preamble_detects_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.dcm");
        let mut bytes = vec![0u8; 132];
        bytes[128..132].copy_from_slice(b"DICM");
        std::fs::write(&good, &bytes).unwrap();
        assert!(has_dicom_preamble(&good));

        let bad = tmp.path().join("bad.dcm");
        std::fs::write(&bad, vec![0u8; 200]).unwrap();
        assert!(!has_dicom_preamble(&bad));

        // Missing file -> false (no panic).
        assert!(!has_dicom_preamble(&tmp.path().join("missing.dcm")));
    }

    #[test]
    fn collect_dicom_files_finds_extensions_and_preamble() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.path().join("a.dcm"), b"x").unwrap();
        std::fs::write(sub.join("b.dcm"), b"x").unwrap();
        std::fs::write(sub.join("notes.txt"), b"x").unwrap();
        // Extensionless file with a DICOM preamble.
        let mut bytes = vec![0u8; 132];
        bytes[128..132].copy_from_slice(b"DICM");
        std::fs::write(sub.join("IM0001"), &bytes).unwrap();

        let found = collect_dicom_files_recursively(vec![tmp.path().to_path_buf()]);
        assert_eq!(found.len(), 3, "found {found:?}");
        // A direct file path is returned as-is when supported.
        let single = collect_dicom_files_recursively(vec![tmp.path().join("a.dcm")]);
        assert_eq!(single.len(), 1);
        // Unsupported direct file is dropped.
        assert!(collect_dicom_files_recursively(vec![sub.join("notes.txt")]).is_empty());
    }

    // ── Real DICOM end-to-end (decode + load + preview + headless UI) ───────

    /// Write a small multi-slice CT series to `dir`.
    fn write_dicom_series(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        use dicom_core::value::PrimitiveValue;
        use dicom_core::VR;
        use dicom_object::mem::InMemElement;
        use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, Tag};

        let (rows, cols, nz) = (16usize, 16usize, 8usize);
        let spacing = 2.0f64;
        let mut paths = Vec::new();
        for z in 0..nz {
            let sop = format!("1.2.826.0.1.3680043.10.77.{z}");
            let meta = FileMetaTableBuilder::new()
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.2")
                .media_storage_sop_instance_uid(&sop)
                .transfer_syntax("1.2.840.10008.1.2.1")
                .build()
                .unwrap();
            let mut obj = DefaultDicomObject::new_empty_with_meta(meta);
            let mut data = vec![-1000i16; rows * cols];
            for y in 0..rows {
                for x in 0..cols {
                    let wx = x as f64 - 8.0;
                    let wy = y as f64 - 8.0;
                    let wz = z as f64 - 4.0;
                    if (wx / 6.0).powi(2) + (wy / 6.0).powi(2) + (wz / 8.0).powi(2) <= 1.0 {
                        data[y * cols + x] = 40;
                    }
                }
            }
            let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.2");
            let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, &sop);
            let _ = obj.put_str(Tag(0x0008, 0x0060), VR::CS, "CT");
            let _ = obj.put_str(Tag(0x0020, 0x000D), VR::UI, "1.2.826.0.1.3680043.10.77.0");
            let _ = obj.put_str(Tag(0x0020, 0x000E), VR::UI, "1.2.826.0.1.3680043.10.77.1");
            let _ = obj.put_str(Tag(0x0020, 0x0011), VR::IS, "1");
            let _ = obj.put_str(Tag(0x0020, 0x0013), VR::IS, &(z as i32 + 1).to_string());
            let _ = obj.put_str(
                Tag(0x0020, 0x0032),
                VR::DS,
                &format!("{:.3}\\{:.3}\\{:.3}", -16.0, -16.0, z as f64 * spacing),
            );
            let _ = obj.put_str(Tag(0x0020, 0x0037), VR::DS, "1\\0\\0\\0\\1\\0");
            let _ = obj.put_str(Tag(0x0028, 0x0030), VR::DS, "2\\2");
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0002), VR::US, PrimitiveValue::new_u16(1)));
            let _ = obj.put_str(Tag(0x0028, 0x0004), VR::CS, "MONOCHROME2");
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0010), VR::US, PrimitiveValue::new_u16(rows as u16)));
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0011), VR::US, PrimitiveValue::new_u16(cols as u16)));
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0100), VR::US, PrimitiveValue::new_u16(16)));
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0101), VR::US, PrimitiveValue::new_u16(16)));
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0102), VR::US, PrimitiveValue::new_u16(15)));
            let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0103), VR::US, PrimitiveValue::new_u16(1)));
            let _ = obj.put_str(Tag(0x0028, 0x1052), VR::DS, "0");
            let _ = obj.put_str(Tag(0x0028, 0x1053), VR::DS, "1");
            let _ = obj.put_str(Tag(0x0018, 0x0088), VR::DS, "2");
            let pixel: InMemElement =
                InMemElement::new(Tag(0x7FE0, 0x0010), VR::OW, PrimitiveValue::I16(data.into()));
            let _ = obj.put(pixel);

            let path = dir.join(format!("slice_{z:03}.dcm"));
            obj.write_to_file(&path).unwrap();
            paths.push(path);
        }
        paths
    }

    #[test]
    fn decode_single_file_reads_pixels_and_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = write_dicom_series(tmp.path());
        let image = decode_single_file(&paths[0]).expect("decode ok");
        assert_eq!(image.raw_image.dimensions(), (16, 16));
        assert!(image.raw_image.is_grayscale16());
        assert_eq!(image.pixel_spacing, Some([2.0, 2.0]));
        assert!(image.image_position_patient.is_some());
        assert!(image.image_orientation_patient.is_some());
        assert!(image.thumbnail.is_some());
        // Modality is captured in metadata.
        assert!(image.metadata.iter().any(|(k, v)| k == "Modality" && v == "CT"));
        // Invalid file yields an error, not a panic.
        let junk = tmp.path().join("junk.dcm");
        std::fs::write(&junk, b"nope").unwrap();
        assert!(decode_single_file(&junk).is_err());
    }

    #[test]
    fn decode_single_file_reads_rgb_pixels() {
        use dicom_core::value::PrimitiveValue;
        use dicom_core::VR;
        use dicom_object::mem::InMemElement;
        use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, Tag};

        let tmp = tempfile::tempdir().unwrap();
        let meta = FileMetaTableBuilder::new()
            .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.7")
            .media_storage_sop_instance_uid("1.2.826.0.1.3680043.10.88.1")
            .transfer_syntax("1.2.840.10008.1.2.1")
            .build()
            .unwrap();
        let mut obj = DefaultDicomObject::new_empty_with_meta(meta);
        let _ = obj.put_str(Tag(0x0008, 0x0060), VR::CS, "OT");
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0002), VR::US, PrimitiveValue::new_u16(3)));
        let _ = obj.put_str(Tag(0x0028, 0x0004), VR::CS, "RGB");
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0010), VR::US, PrimitiveValue::new_u16(1)));
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0011), VR::US, PrimitiveValue::new_u16(2)));
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0100), VR::US, PrimitiveValue::new_u16(8)));
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0101), VR::US, PrimitiveValue::new_u16(8)));
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0102), VR::US, PrimitiveValue::new_u16(7)));
        let _ = obj.put(InMemElement::new(Tag(0x0028, 0x0103), VR::US, PrimitiveValue::new_u16(0)));
        let pixels: InMemElement = InMemElement::new(
            Tag(0x7FE0, 0x0010),
            VR::OB,
            PrimitiveValue::U8(vec![10, 20, 30, 40, 50, 60].into()),
        );
        let _ = obj.put(pixels);
        let path = tmp.path().join("rgb.dcm");
        obj.write_to_file(&path).unwrap();

        let image = decode_single_file(&path).expect("decode rgb");
        assert_eq!(image.raw_image.dimensions(), (2, 1));
        assert!(!image.raw_image.is_grayscale16());
        match image.raw_image {
            RawImage::Rgb8 { data, .. } => assert_eq!(data, vec![10, 20, 30, 40, 50, 60]),
            _ => panic!("expected Rgb8"),
        }
    }

    #[test]
    fn mpr_preview_from_dir_renders_and_validates() {
        let tmp = tempfile::tempdir().unwrap();
        write_dicom_series(tmp.path());
        for plane in ["axial", "coronal", "sagittal"] {
            let text = mpr_preview_from_dir(tmp.path(), plane, None).expect("preview");
            assert!(text.contains("top=row0"), "header missing for {plane}");
            assert!(text.lines().count() > 5);
        }
        assert!(mpr_preview_from_dir(tmp.path(), "oblique", None).is_err());
        // A directory without enough slices is rejected.
        let empty = tempfile::tempdir().unwrap();
        assert!(mpr_preview_from_dir(empty.path(), "axial", None).is_err());
    }

    #[test]
    fn app_load_files_populates_and_renders_single_viewport() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = write_dicom_series(tmp.path());
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        app.load_files(paths, false, &ctx);
        // Loading runs on a worker thread; wait for it to finish.
        let mut spins = 0;
        while app.loading.is_some() && spins < 600 {
            app.poll_loading(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
            spins += 1;
        }
        assert_eq!(app.images.len(), 8);
        assert_eq!(app.series_groups.len(), 1);
        assert!(app.active_image().is_some());

        // Render one frame (single viewport) to exercise the draw + overlay path.
        run_headless_frame(&mut app, &ctx);

        // Switch to MPR and render again.
        app.vp_mut().view_mode = ViewMode::Mpr;
        app.refresh_mpr_volumes();
        run_headless_frame(&mut app, &ctx);
    }

    #[test]
    fn app_renders_multiviewport_and_overlays() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = write_dicom_series(tmp.path());
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        app.load_files(paths, false, &ctx);
        let mut spins = 0;
        while app.loading.is_some() && spins < 600 {
            app.poll_loading(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
            spins += 1;
        }
        // Two viewports, one Stack one MPR, both with overlays.
        app.viewports.push(ViewportState::default());
        app.viewport_textures.push(None);
        app.viewports[1].view_mode = ViewMode::Mpr;
        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);
        app.refresh_mpr_volumes();
        run_headless_frame(&mut app, &ctx);
        // The deface action exercises the in-memory pipeline from the UI model.
        app.active_viewport = 0;
        app.deface_active_series(&ctx);
        assert!(app.series_groups.iter().any(|g| g.uid.ends_with("-DEFACED")));
        run_headless_frame(&mut app, &ctx);
    }

    #[test]
    fn programmatic_loads_do_not_touch_recent_history() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        let paths = write_dicom_series(tmp.path());
        let paths_dir = paths[0].parent().unwrap().to_path_buf();
        let ctx = egui::Context::default();
        let mut app = DicomViewApp::new(None);
        app.recent_folders.clear();

        // A programmatic load (CLI/`diface --view`/tests) must NOT be recorded.
        app.load_files(paths.clone(), false, &ctx);
        let mut spins = 0;
        while app.loading.is_some() && spins < 600 {
            app.poll_loading(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
            spins += 1;
        }
        assert!(
            app.recent_folders.is_empty(),
            "programmatic load should not record a recent folder, got {:?}",
            app.recent_folders
        );

        // A user-initiated load IS recorded.
        app.load_files(paths, true, &ctx);
        spins = 0;
        while app.loading.is_some() && spins < 600 {
            app.poll_loading(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
            spins += 1;
        }
        assert_eq!(
            app.recent_folders.len(),
            1,
            "user-initiated load should record a recent folder"
        );
        assert_eq!(app.recent_folders[0], paths_dir);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn recent_folder_helpers_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        let mut app = DicomViewApp::new(None);
        app.recent_folders.clear();
        app.push_recent_folder(std::path::PathBuf::from("/data/one"));
        app.push_recent_folder(std::path::PathBuf::from("/data/two"));
        app.push_recent_folder(std::path::PathBuf::from("/data/one")); // moves to front
        assert_eq!(app.recent_folders.first().unwrap(), &std::path::PathBuf::from("/data/one"));
        assert_eq!(app.recent_folders.len(), 2, "duplicates are not added");
        app.save_recent_folders();
        let reloaded = DicomViewApp::load_recent_folders();
        assert!(reloaded.contains(&std::path::PathBuf::from("/data/one")));
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn active_mpr_error_reports_for_rgb_series() {
        let mut app = DicomViewApp::new(None);
        app.images = vec![LoadedImage {
            raw_image: RawImage::Rgb8 { data: vec![0, 0, 0, 0, 0, 0], width: 2, height: 1 },
            metadata: Vec::new(),
            filename: "rgb".to_string(),
            series_uid: "rgb".to_string(),
            series_label: "RGB".to_string(),
            instance_number: Some(1),
            default_wc_ww: None,
            study_uid: None,
            thumbnail: None,
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: None,
            spacing_between_slices: None,
            image_position_patient: Some([0.0, 0.0, 0.0]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        }];
        app.rebuild_series_groups();
        app.refresh_mpr_volumes();
        // Single slice + RGB -> MPR unavailable and an error is recorded.
        assert!(!app.mpr_available());
        assert!(app.active_mpr_error().is_some());
    }

    #[test]
    fn align_active_to_group_snaps_to_existing_member() {
        let ctx = egui::Context::default();
        let (a, b) = two_head_series();
        let mut app = DicomViewApp::new(None);
        app.images = a;
        app.images.extend(b);
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.viewports[0].current_series = app.series_groups.iter().position(|g| g.uid == "A").unwrap();
        app.viewports[1].current_series = app.series_groups.iter().position(|g| g.uid == "B").unwrap();

        // Viewport 0 in group 0 at slice 6; viewport 1 joins group 0.
        app.viewports[0].sync_group = Some(0);
        app.viewports[0].current_stack_slice = 6;
        app.active_viewport = 1;
        app.viewports[1].sync_group = Some(0);
        app.align_active_to_group(&ctx);
        assert_eq!(app.viewports[1].current_stack_slice, 6);

        // An ungrouped viewport is left alone.
        app.viewports[1].sync_group = None;
        app.viewports[1].current_stack_slice = 0;
        app.align_active_to_group(&ctx);
        assert_eq!(app.viewports[1].current_stack_slice, 0);
    }

    #[test]
    fn metadata_image_falls_back_to_first_series_image() {
        // A viewport whose current_stack_slice is out of range has no active
        // image, but metadata_image() still returns the series' first image.
        let mut app = app_with_head();
        app.vp_mut().current_stack_slice = 9999;
        assert!(app.active_image().is_none());
        assert!(app.metadata_image().is_some());
    }

    #[test]
    fn slice_position_falls_back_to_instance_number() {
        let mut image = gray16_image("s", 7, [0.0, 0.0, 0.0], [1.0, 1.0], 2, 2, &[0.0; 4]);
        image.image_position_patient = None;
        image.image_orientation_patient = None;
        assert!((image.slice_position() - 7.0).abs() < 1e-6);
    }

    #[test]
    fn series_selector_change_resets_slices() {
        let ctx = egui::Context::default();
        let (a, b) = two_head_series();
        let mut app = DicomViewApp::new(None);
        app.images = a;
        app.images.extend(b);
        app.rebuild_series_groups();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.viewports[0].current_series = app.series_groups.iter().position(|g| g.uid == "A").unwrap();
        app.viewports[0].current_stack_slice = 5;

        // Simulate the selector switching to the other series.
        let target = app.series_groups.iter().position(|g| g.uid == "B").unwrap();
        app.viewports[0].current_series = target;
        app.viewports[0].current_stack_slice = 0;
        app.refresh_mpr_volumes();
        app.apply_default_window_for_current_series();
        app.update_slice_view_for(0, &ctx);
        assert_eq!(app.viewports[0].current_stack_slice, 0);
    }

    #[test]
    fn window_level_overlay_reports_windowing_and_edit_state() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.wl_editor_open = vec![false];

        // Render the W/L overlay for a grayscale (windowing) viewport.
        let mut toggled = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            toggled = app.window_level_overlay_ui(ui, 0);
        });
        output.textures_delta.clear();
        assert!(!toggled, "no double-click means no toggle");

        // With the editor open, the WC/WW fields render too.
        app.wl_editor_open = vec![true];
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = app.window_level_overlay_ui(ui, 0);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn switching_to_mpr_keeps_window_level_preset() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.viewports[0].view_mode = ViewMode::Stack;
        app.viewports[0].window_center = 1234.0;
        app.viewports[0].window_width = 567.0;

        // Drive the view-mode controls; the mode flips Stack -> MPR.
        app.viewports[0].view_mode = ViewMode::Mpr;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.view_mode_controls_ui(ui, 0);
        });
        output.textures_delta.clear();

        assert_eq!(app.viewports[0].view_mode, ViewMode::Mpr);
        assert_eq!(
            (app.viewports[0].window_center, app.viewports[0].window_width),
            (1234.0, 567.0),
            "MPR must keep the current window/level preset"
        );
    }

    #[test]
    fn deface_alignment_panel_renders_and_applies() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.deface_panel_open = true;

        // Render the alignment panel.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.deface_alignment_ui(ui);
        });
        output.textures_delta.clear();

        // Editing params then defacing uses them.
        app.deface_params.yaw_deg = 45.0;
        app.deface_params.extent_scale = [1.2, 1.0, 0.8];
        app.deface_active_series(&ctx);
        assert!(app.series_groups.iter().any(|g| g.uid.ends_with("-DEFACED")));

        // Auto reset restores defaults (the Balanced preset).
        app.deface_params = GeometricParams::default();
        assert_eq!(app.deface_params.yaw_deg, 0.0);
        assert_eq!(
            app.deface_params.matching_preset(),
            Some(DefacePreset::Balanced)
        );
    }

    #[test]
    fn deface_preview_toggles_and_tints() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;

        assert!(app.deface_preview_on);
        assert!(app.deface_preview.is_none());
        // Turning it off then on rebuilds the preview.
        app.toggle_deface_preview(&ctx);
        assert!(!app.deface_preview_on && app.deface_preview.is_none());
        app.toggle_deface_preview(&ctx);
        let preview = app.deface_preview.as_ref().expect("preview on");
        assert_eq!(preview.uid, app.series_groups[0].uid);

        // Every slice in the series has a per-image flag buffer of the right
        // length, and the 3D mask matches the volume dims.
        let group = &app.series_groups[0];
        for gi in &group.image_indices {
            let flags = preview.per_image.get(gi).expect("per-image flags");
            assert_eq!(flags.len(), 32 * 32);
        }
        assert_eq!(preview.dims, [32, 32, 20]);
        assert_eq!(preview.volume_flags.len(), 32 * 32 * 20);
        // Some voxels are flagged (the face).
        assert!(preview.volume_flags.iter().any(|&b| b));

        // Toggling off clears it.
        app.toggle_deface_preview(&ctx);
        assert!(app.deface_preview.is_none());
    }

    #[test]
    fn preview_plane_flags_match_volume_dims() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.refresh_deface_preview(&ctx);
        let preview = app.deface_preview.clone().unwrap();

        for plane in [MprPlane::Axial, MprPlane::Coronal, MprPlane::Sagittal] {
            let len = preview.dims[plane.patient_axes().2.index()];
            let idx = len / 2;
            let flags = preview.plane_flags(plane, idx);
            let (u, v, _) = plane.patient_axes();
            assert_eq!(flags.len(), preview.dims[u.index()] * preview.dims[v.index()]);
        }

        // An MPR viewport on the base series renders the tinted preview without
        // panicking, and its texture is rebuilt.
        app.viewports[0].view_mode = ViewMode::Mpr;
        app.viewports[0].mpr_plane = MprPlane::Sagittal;
        app.refresh_mpr_volumes();
        let series = app.viewports[0].current_series;
        app.update_slice_view_for(0, &ctx);
        assert!(app.viewport_textures.get(0).and_then(|o| o.as_ref()).is_some());
        assert_eq!(app.mpr_series_for(0), Some(series));
    }

    #[test]
    fn segmentation_areas_render_in_preview() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.deface_show_segmentation = true;
        app.refresh_deface_preview(&ctx);

        let preview = app.deface_preview.as_ref().expect("preview");
        let labels = preview.volume_labels.as_ref().expect("labels present");
        assert_eq!(labels.len(), preview.volume_flags.len());
        // The overlay should contain more than one distinct label (at least
        // head and brain, and typically skull on CT).
        let mut seen = std::collections::HashSet::new();
        for &l in labels {
            seen.insert(l);
        }
        assert!(seen.len() >= 2, "expected several areas, saw {seen:?}");

        // Plane labels map onto MPR.
        let pl = preview.plane_labels(MprPlane::Axial, preview.dims[2] / 2);
        assert!(pl.is_some());

        // tint_segmentation colours a labelled buffer.
        let mut rgba = vec![100u8; 4 * 4];
        tint_segmentation(&mut rgba, &[1, 2, 3, 4]);
        assert_ne!(&rgba[0..4], &[100, 100, 100, 100]); // head tinted
        assert_ne!(&rgba[12..16], &[100, 100, 100, 100]); // cavity tinted
        // A zero label is left untouched.
        let mut rgba0 = vec![100u8; 4];
        tint_segmentation(&mut rgba0, &[0]);
        assert_eq!(&rgba0[0..4], &[100, 100, 100, 100]);
    }

    #[test]
    fn deflesh_region_removes_more_than_face() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.deface_backend = DefaceBackendSel::Segmentation;

        app.deface_seg_region = diface_rs::MaskRegion::Face;
        let (_, face) = app
            .compute_deface_mask(&app.series_groups[0].clone())
            .expect("face mask");
        app.deface_seg_region = diface_rs::MaskRegion::ExternalSoftTissue;
        let (_, deflesh) = app
            .compute_deface_mask(&app.series_groups[0].clone())
            .expect("deflesh mask");
        assert!(
            deflesh.count_removed() > face.count_removed(),
            "deflesh ({}) should remove more than face ({})",
            deflesh.count_removed(),
            face.count_removed()
        );

        // The panel renders with the Region selector without panicking.
        app.deface_panel_open = true;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.deface_alignment_ui(ui);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn tint_removed_blends_flagged_pixels_only() {
        let mut rgba = vec![100u8, 100, 100, 255, 100, 100, 100, 255];
        tint_removed(&mut rgba, &[true, false]);
        // Flagged pixel shifts toward red.
        assert!(rgba[0] > 100 && rgba[1] < 100 && rgba[2] < 100);
        // Unflagged pixel is untouched.
        assert_eq!(&rgba[4..8], &[100, 100, 100, 255]);
    }

    #[test]
    fn deface_algorithm_selection_and_preview() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        assert_eq!(app.deface_params.algorithm, DefaceAlgorithm::Ellipsoid);

        // Turning on the preview then changing algorithm recomputes the mask.
        app.refresh_deface_preview(&ctx);
        let ellipsoid_removed = app
            .deface_preview
            .as_ref()
            .unwrap()
            .volume_flags
            .iter()
            .filter(|b| **b)
            .count();
        app.deface_params.algorithm = DefaceAlgorithm::Plane;
        app.refresh_deface_preview(&ctx);
        let plane_removed = app
            .deface_preview
            .as_ref()
            .unwrap()
            .volume_flags
            .iter()
            .filter(|b| **b)
            .count();
        assert_ne!(
            ellipsoid_removed, plane_removed,
            "algorithm switch must change the previewed mask"
        );

        // The panel renders with the algorithm dropdown.
        app.deface_panel_open = true;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.deface_alignment_ui(ui);
        });
        output.textures_delta.clear();

        // Applying a preset keeps the chosen algorithm.
        app.deface_params.apply_preset(DefacePreset::Thorough);
        assert_eq!(app.deface_params.algorithm, DefaceAlgorithm::Plane);
    }

    #[test]
    fn atlas_backend_path_produces_a_mask_and_previews() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        // Use the atlas backend with the built-in synthetic template.
        app.deface_backend = DefaceBackendSel::Atlas;

        let (_, mask) = app
            .compute_deface_mask(&app.series_groups[0].clone())
            .expect("atlas mask");
        assert!(mask.count_removed() > 0);

        // Preview + deface work with the atlas backend selected.
        app.refresh_deface_preview(&ctx);
        assert!(app.deface_preview.is_some());
        app.deface_active_series(&ctx);
        assert!(app.series_groups.iter().any(|g| g.uid.ends_with("-DEFACED")));
    }

    #[test]
    fn deface_presets_change_settings_and_keep_orientation() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.deface_params.yaw_deg = 25.0;

        // Default matches Balanced.
        assert_eq!(app.deface_params.matching_preset(), Some(DefacePreset::Balanced));

        // Selecting Conservative updates sizing but keeps the manual yaw.
        app.deface_params.apply_preset(DefacePreset::Conservative);
        assert_eq!(app.deface_params.matching_preset(), Some(DefacePreset::Conservative));
        assert_eq!(app.deface_params.yaw_deg, 25.0);

        // The panel renders with presets and stays usable.
        app.deface_panel_open = true;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.deface_alignment_ui(ui);
        });
        output.textures_delta.clear();

        // Presets remove progressively less/more.
        let removed = |p: DefacePreset| {
            let mut a = app_with_head();
            a.deface_params.apply_preset(p);
            a.deface_active_series(&ctx);
            let idx = a.series_groups.iter().position(|g| g.uid.ends_with("-DEFACED")).unwrap();
            let gi = a.series_groups[idx].image_indices[10];
            match &a.images[gi].raw_image {
                RawImage::Gray16(g) => g.data.iter().filter(|v| **v == -1000.0).count(),
                _ => 0,
            }
        };
        assert!(removed(DefacePreset::Conservative) < removed(DefacePreset::Thorough));
    }

    #[test]
    fn reset_view_clears_preview_and_view_state() {
        let ctx = egui::Context::default();
        let mut app = app_with_head();
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        app.refresh_deface_preview(&ctx);
        assert!(app.deface_preview.is_some());

        // Simulate the reset action's effect.
        app.vp_mut().zoom = 3.0;
        app.vp_mut().rotation_degrees = 45.0;
        app.vp_mut().pan = egui::vec2(20.0, 20.0);
        app.deface_preview = None;
        app.vp_mut().zoom = 1.0;
        app.vp_mut().rotation_degrees = 0.0;
        app.vp_mut().pan = Vec2::ZERO;

        assert!(app.deface_preview.is_none());
        assert_eq!(app.vp().zoom, 1.0);
        assert_eq!(app.vp().rotation_degrees, 0.0);
        assert_eq!(app.vp().pan, Vec2::ZERO);
    }

    /// Run one `eframe::App::ui` frame headlessly with the given raw input.
    fn run_headless_frame_with(app: &mut DicomViewApp, ctx: &egui::Context, raw: egui::RawInput) {
        use eframe::App as _;
        let mut output = ctx.run_ui(raw, |ui| {
            app.ui(ui, &mut eframe::Frame::_new_kittest());
        });
        // We are not a real renderer, so drop any texture uploads cleanly.
        output.textures_delta.clear();
    }

    /// Run one `eframe::App::ui` frame headlessly.
    fn run_headless_frame(app: &mut DicomViewApp, ctx: &egui::Context) {
        run_headless_frame_with(app, ctx, egui::RawInput::default());
    }

    fn loaded_app(ctx: &egui::Context) -> DicomViewApp {
        let tmp = tempfile::tempdir().unwrap();
        let paths = write_dicom_series(tmp.path());
        let mut app = DicomViewApp::new(None);
        app.load_files(paths, false, ctx);
        let mut spins = 0;
        while app.loading.is_some() && spins < 600 {
            app.poll_loading(ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
            spins += 1;
        }
        app
    }

    #[test]
    fn frame_handles_pointer_drag_scroll_and_keyboard() {
        let ctx = egui::Context::default();
        let mut app = loaded_app(&ctx);
        app.viewports = vec![ViewportState::default()];
        app.active_viewport = 0;
        // Give the pass a real screen size so the central panel has area.
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let mouse = |pos: egui::Pos2, down: bool| egui::PointerButton::Primary;
        let _ = mouse; // silence unused in case of cfg

        // Frame 1: pointer down + move (pan drag), then primary down.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerMoved(egui::pos2(400.0, 300.0)));
        raw.events.push(egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        raw.events.push(egui::Event::PointerMoved(egui::pos2(430.0, 320.0)));
        run_headless_frame_with(&mut app, &ctx, raw);

        // Frame 2: scroll wheel over the image (slice navigation / zoom).
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerMoved(egui::pos2(400.0, 300.0)));
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        });
        run_headless_frame_with(&mut app, &ctx, raw);

        // Frame 3: keyboard navigation + drag-and-drop hover.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        raw.events.push(egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
        run_headless_frame_with(&mut app, &ctx, raw);

        // The app survived a frame in Stack mode with no panic.
        assert!(app.active_image().is_some());
    }

    #[test]
    fn frame_renders_mpr_multiviewport_with_crossrefs() {
        let ctx = egui::Context::default();
        let mut app = loaded_app(&ctx);
        app.viewports = vec![ViewportState::default(), ViewportState::default()];
        app.viewport_textures = vec![None, None];
        app.active_viewport = 0;
        app.viewports[0].view_mode = ViewMode::Mpr;
        app.viewports[0].mpr_plane = MprPlane::Axial;
        app.viewports[1].view_mode = ViewMode::Mpr;
        app.viewports[1].mpr_plane = MprPlane::Coronal;
        app.viewports[0].sync_group = Some(0);
        app.viewports[1].sync_group = Some(0);
        app.refresh_mpr_volumes();
        // Two frames so both viewports' textures/cross-refs are built.
        run_headless_frame(&mut app, &ctx);
        run_headless_frame(&mut app, &ctx);
        assert!(app.active_mpr_volume().is_some());
    }

    #[test]
    fn frame_handles_empty_app_and_toolbar_paths() {
        let ctx = egui::Context::default();
        // No images: exercises the "no DICOM files" / open-files toolbar paths.
        let mut app = DicomViewApp::new(None);
        app.recent_folders = vec![std::path::PathBuf::from("/nowhere")];
        run_headless_frame(&mut app, &ctx);

        // Error state renders the error panel.
        app.error = Some("boom".to_string());
        run_headless_frame(&mut app, &ctx);

        // Loading state renders the progress modal.
        app.error = None;
        let (_tx, rx) = mpsc::channel::<LoadMsg>();
        app.loading = Some(LoadingState {
            rx,
            total: 3,
            received: 1,
            current_filename: "x.dcm".to_string(),
        });
        run_headless_frame(&mut app, &ctx);
        let _ = app.poll_loading(&ctx);
    }
}

pub fn run_viewer() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DICOM Viewer")
            .with_inner_size([1024.0, 768.0]),
        // Use the wgpu renderer instead of the default glow/glutin one. The glow
        // backend fails to create a GLX context on some displays (glutin
        // "BadValue"), while wgpu can fall back to a working backend (e.g.
        // Vulkan), so the window opens reliably across environments.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "DICOM Viewer",
        options,
        Box::new(|_cc| Ok(Box::new(DicomViewApp::new(None)))),
    )
    .expect("failed to launch DICOM Viewer");
}

pub fn run_viewer_with_files(paths: Vec<String>) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DICOM Viewer")
            .with_inner_size([1024.0, 768.0]),
        // Use the wgpu renderer instead of the default glow/glutin one. The glow
        // backend fails to create a GLX context on some displays (glutin
        // "BadValue"), while wgpu can fall back to a working backend (e.g.
        // Vulkan), so the window opens reliably across environments.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let path_bufs = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    eframe::run_native(
        "DICOM Viewer",
        options,
        Box::new(move |_cc| {
            let mut app = DicomViewApp::new(None);
            // Files passed on the command line (e.g. `diface --view`) are not
            // recorded in the recent-folders history.
            app.pending_load = Some((path_bufs, false));
            Ok(Box::new(app))
        }),
    )
    .expect("failed to launch DICOM Viewer");
}
