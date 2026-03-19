use eframe::egui;
use egui::Vec2;
use dicom_object::{open_file, Tag};
use dicom_pixeldata::PixelDecoder;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;

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

/// In-memory decoded pixels before rendering (allows efficient W/L re-renders).
struct Gray16 {
    data: Vec<f32>, // rescaled (HU for CT, raw*slope+intercept for others)
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

    fn patient_axes(self) -> (PatientAxis, PatientAxis, PatientAxis) {
        match self {
            MprPlane::Axial => (PatientAxis::X, PatientAxis::Y, PatientAxis::Z),
            MprPlane::Coronal => (PatientAxis::X, PatientAxis::Z, PatientAxis::Y),
            MprPlane::Sagittal => (PatientAxis::Y, PatientAxis::Z, PatientAxis::X),
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

#[derive(Debug)]
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
        let column_step = scale(column_dir, pixel_spacing[1].abs().max(0.001));
        let row_step = scale(row_dir, pixel_spacing[0].abs().max(0.001));
        let slice_step = scale(slice_dir, slice_spacing.abs().max(0.001));
        let max_column = width.saturating_sub(1) as f32;
        let max_row = height.saturating_sub(1) as f32;
        let max_slice = slices.len().saturating_sub(1) as f32;
        let corner_offsets = [
            [0.0, 0.0, 0.0],
            [max_column, 0.0, 0.0],
            [0.0, max_row, 0.0],
            [max_column, max_row, 0.0],
            [0.0, 0.0, max_slice],
            [max_column, 0.0, max_slice],
            [0.0, max_row, max_slice],
            [max_column, max_row, max_slice],
        ];
        let mut patient_min = [f32::MAX; 3];
        let mut patient_max = [f32::MIN; 3];
        for [column_idx, row_idx, slice_idx] in corner_offsets {
            let point = add(
                origin,
                add(
                    scale(column_step, column_idx),
                    add(scale(row_step, row_idx), scale(slice_step, slice_idx)),
                ),
            );
            for axis in 0..3 {
                patient_min[axis] = patient_min[axis].min(point[axis]);
                patient_max[axis] = patient_max[axis].max(point[axis]);
            }
        }
        let axis_spacing = |axis: PatientAxis| {
            let unit = axis.unit();
            let candidates = [
                pixel_spacing[1].abs().max(0.001) * dot(column_dir, unit).abs(),
                pixel_spacing[0].abs().max(0.001) * dot(row_dir, unit).abs(),
                slice_spacing.abs().max(0.001) * dot(slice_dir, unit).abs(),
            ];
            let best = candidates
                .into_iter()
                .filter(|value| *value > 0.125)
                .fold(f32::MAX, f32::min);
            if best.is_finite() {
                best.max(0.125)
            } else {
                pixel_spacing[0]
                    .abs()
                    .min(pixel_spacing[1].abs())
                    .min(slice_spacing.abs())
                    .max(0.125)
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

    fn sample_trilinear(&self, patient_point: [f32; 3]) -> f32 {
        let delta = sub(patient_point, self.origin);
        let column = dot(delta, self.column_dir) / self.col_spacing;
        let row = dot(delta, self.row_dir) / self.row_spacing;
        let slice = dot(delta, self.slice_dir) / self.slice_spacing;

        if column < 0.0
            || row < 0.0
            || slice < 0.0
            || column > self.width.saturating_sub(1) as f32
            || row > self.height.saturating_sub(1) as f32
            || slice > self.depth.saturating_sub(1) as f32
        {
            return self.background_value;
        }

        let c0 = column.floor() as usize;
        let r0 = row.floor() as usize;
        let s0 = slice.floor() as usize;
        let c1 = (c0 + 1).min(self.width.saturating_sub(1));
        let r1 = (r0 + 1).min(self.height.saturating_sub(1));
        let s1 = (s0 + 1).min(self.depth.saturating_sub(1));
        let dc = column - c0 as f32;
        let dr = row - r0 as f32;
        let ds = slice - s0 as f32;
        let idx = |c: usize, r: usize, s: usize| s * self.width * self.height + r * self.width + c;

        let c00 = self.data[idx(c0, r0, s0)] * (1.0 - dc) + self.data[idx(c1, r0, s0)] * dc;
        let c01 = self.data[idx(c0, r0, s1)] * (1.0 - dc) + self.data[idx(c1, r0, s1)] * dc;
        let c10 = self.data[idx(c0, r1, s0)] * (1.0 - dc) + self.data[idx(c1, r1, s0)] * dc;
        let c11 = self.data[idx(c0, r1, s1)] * (1.0 - dc) + self.data[idx(c1, r1, s1)] * dc;
        let c0v = c00 * (1.0 - dr) + c10 * dr;
        let c1v = c01 * (1.0 - dr) + c11 * dr;
        c0v * (1.0 - ds) + c1v * ds
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

        for row_idx in 0..height {
            let v_coord = self.axis_coordinate(v_axis, row_idx);
            for col_idx in 0..width {
                let u_coord = self.axis_coordinate(u_axis, col_idx);
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
struct LoadedImage {
    raw_image: RawImage,
    metadata: Vec<(String, String)>,
    filename: String,
    series_uid: String,
    series_label: String,
    instance_number: Option<i32>,
    default_wc_ww: Option<(f32, f32)>,
    pixel_spacing: Option<[f32; 2]>,
    slice_thickness: Option<f32>,
    spacing_between_slices: Option<f32>,
    image_position_patient: Option<[f32; 3]>,
    image_orientation_patient: Option<[f32; 6]>,
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

struct SeriesGroup {
    uid: String,
    label: String,
    image_indices: Vec<usize>,
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

    Ok(LoadedImage {
        raw_image,
        metadata,
        filename,
        series_uid,
        series_label,
        instance_number,
        default_wc_ww,
        pixel_spacing,
        slice_thickness,
        spacing_between_slices,
        image_position_patient,
        image_orientation_patient,
    })
}

struct DicomViewApp {
    pending_load: Option<Vec<PathBuf>>,
    images: Vec<LoadedImage>,
    series_groups: Vec<SeriesGroup>,
    current_series: usize,
    current_stack_slice: usize,
    current_axial_slice: usize,
    current_coronal_slice: usize,
    current_sagittal_slice: usize,
    view_mode: ViewMode,
    mpr_plane: MprPlane,
    mpr_volume: Option<MprVolume>,
    mpr_error: Option<String>,
    texture: Option<egui::TextureHandle>,
    displayed_physical_size: Option<Vec2>,
    zoom: f32,
    rotation_degrees: f32,
    pan: Vec2,
    error: Option<String>,
    show_metadata: bool,
    window_center: f32,
    window_width: f32,
    wl_dirty: bool,
    files_hovered: bool,
    loading: Option<LoadingState>,
}

impl DicomViewApp {
    fn new(paths: Option<Vec<PathBuf>>) -> Self {
        let pending_load = paths;
        Self {
            pending_load,
            images: Vec::new(),
            series_groups: Vec::new(),
            current_series: 0,
            current_stack_slice: 0,
            current_axial_slice: 0,
            current_coronal_slice: 0,
            current_sagittal_slice: 0,
            view_mode: ViewMode::Stack,
            mpr_plane: MprPlane::Axial,
            mpr_volume: None,
            mpr_error: None,
            texture: None,
            displayed_physical_size: None,
            zoom: 1.0,
            rotation_degrees: 0.0,
            pan: Vec2::ZERO,
            error: None,
            show_metadata: true,
            window_center: 0.0,
            window_width: 1.0,
            wl_dirty: false,
            files_hovered: false,
            loading: None,
        }
    }

    fn load_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let paths = collect_dicom_files_recursively(paths);
        self.error = None;
        self.images.clear();
        self.series_groups.clear();
        self.current_series = 0;
        self.texture = None;
        self.displayed_physical_size = None;
        self.current_stack_slice = 0;
        self.current_axial_slice = 0;
        self.current_coronal_slice = 0;
        self.current_sagittal_slice = 0;
        self.view_mode = ViewMode::Stack;
        self.mpr_plane = MprPlane::Axial;
        self.mpr_volume = None;
        self.mpr_error = None;
        self.pan = Vec2::ZERO;
        self.zoom = 1.0;
        self.rotation_degrees = 0.0;
        self.wl_dirty = false;

        if paths.is_empty() {
            self.error = Some("No DICOM files found (searched recursively, including extensionless files)".to_string());
            self.loading = None;
            return;
        }

        let total = paths.len();
        let (tx, rx) = mpsc::channel::<LoadMsg>();
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            for path in paths {
                let msg = match decode_single_file(&path) {
                    Ok(image) => LoadMsg::Image(image),
                    Err(e) => LoadMsg::Error(e),
                };
                let _ = tx.send(msg);
                ctx_clone.request_repaint();
            }
        });

        self.loading = Some(LoadingState {
            rx,
            total,
            received: 0,
            current_filename: String::new(),
        });
    }

    fn active_series_len(&self) -> usize {
        self.series_groups
            .get(self.current_series)
            .map(|g| g.image_indices.len())
            .unwrap_or(0)
    }

    fn active_image(&self) -> Option<&LoadedImage> {
        let group = self.series_groups.get(self.current_series)?;
        let global_idx = *group.image_indices.get(self.current_stack_slice)?;
        self.images.get(global_idx)
    }

    fn metadata_image(&self) -> Option<&LoadedImage> {
        self.active_image().or_else(|| {
            let group = self.series_groups.get(self.current_series)?;
            let first = *group.image_indices.first()?;
            self.images.get(first)
        })
    }

    fn mpr_available(&self) -> bool {
        self.mpr_volume.is_some()
    }

    fn current_mpr_slice(&self) -> usize {
        match self.mpr_plane {
            MprPlane::Axial => self.current_axial_slice,
            MprPlane::Coronal => self.current_coronal_slice,
            MprPlane::Sagittal => self.current_sagittal_slice,
        }
    }

    fn set_current_mpr_slice(&mut self, index: usize) {
        match self.mpr_plane {
            MprPlane::Axial => self.current_axial_slice = index,
            MprPlane::Coronal => self.current_coronal_slice = index,
            MprPlane::Sagittal => self.current_sagittal_slice = index,
        }
    }

    fn current_view_slice_len(&self) -> usize {
        match self.view_mode {
            ViewMode::Stack => self.active_series_len(),
            ViewMode::Mpr => self
                .mpr_volume
                .as_ref()
                .map(|volume| volume.plane_len(self.mpr_plane))
                .unwrap_or(0),
        }
    }

    fn apply_default_window_for_current_series(&mut self) {
        match self.view_mode {
            ViewMode::Stack => {
                if let Some(img) = self.active_image() {
                    if let Some((wc, ww)) = img.default_wc_ww {
                        self.window_center = wc;
                        self.window_width = ww;
                    }
                }
            }
            ViewMode::Mpr => {
                if let Some(volume) = &self.mpr_volume {
                    let (wc, ww) = volume.default_wc_ww;
                    self.window_center = wc;
                    self.window_width = ww;
                }
            }
        }
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
        self.series_groups = groups;
        self.current_series = 0;
        self.current_stack_slice = 0;
        self.current_axial_slice = 0;
        self.current_coronal_slice = 0;
        self.current_sagittal_slice = 0;
        self.refresh_mpr_volume();
    }

    fn refresh_mpr_volume(&mut self) {
        self.mpr_volume = None;
        self.mpr_error = None;

        let Some(group) = self.series_groups.get(self.current_series) else {
            return;
        };

        match MprVolume::from_images(&self.images, &group.image_indices) {
            Ok(volume) => {
                let axial_max = volume.plane_len(MprPlane::Axial).saturating_sub(1);
                let coronal_max = volume.plane_len(MprPlane::Coronal).saturating_sub(1);
                let sagittal_max = volume.plane_len(MprPlane::Sagittal).saturating_sub(1);
                self.current_axial_slice = if self.current_axial_slice > axial_max {
                    axial_max / 2
                } else {
                    self.current_axial_slice
                };
                self.current_coronal_slice = if self.current_coronal_slice > coronal_max {
                    coronal_max / 2
                } else {
                    self.current_coronal_slice
                };
                self.current_sagittal_slice = if self.current_sagittal_slice > sagittal_max {
                    sagittal_max / 2
                } else {
                    self.current_sagittal_slice
                };
                if self.current_axial_slice == 0 {
                    self.current_axial_slice = axial_max / 2;
                }
                if self.current_coronal_slice == 0 {
                    self.current_coronal_slice = coronal_max / 2;
                }
                if self.current_sagittal_slice == 0 {
                    self.current_sagittal_slice = sagittal_max / 2;
                }
                self.mpr_volume = Some(volume);
            }
            Err(err) => {
                self.mpr_error = Some(err);
                if self.view_mode == ViewMode::Mpr {
                    self.view_mode = ViewMode::Stack;
                }
            }
        }
    }

    fn update_current_slice_view(&mut self, ctx: &egui::Context) {
        self.texture = None;
        self.displayed_physical_size = None;

        match self.view_mode {
            ViewMode::Stack => {
                if self.active_series_len() == 0 {
                    self.wl_dirty = false;
                    return;
                }
                let Some((width, height, rgba, physical_size)) = self.active_image().map(|img| {
                    let (width, height) = img.raw_image.dimensions();
                    (
                        width,
                        height,
                        img.raw_image.to_rgba(self.window_center, self.window_width),
                        img.physical_size(),
                    )
                }) else {
                    self.wl_dirty = false;
                    return;
                };
                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                self.texture = Some(ctx.load_texture(
                    "dicom_image",
                    color_image,
                    egui::TextureOptions::LINEAR,
                ));
                self.displayed_physical_size = Some(physical_size);
            }
            ViewMode::Mpr => {
                let Some(volume) = &self.mpr_volume else {
                    self.wl_dirty = false;
                    return;
                };
                let plane_index = self.current_mpr_slice();
                let pixels = volume.extract_plane(self.mpr_plane, plane_index);
                let (width, height) = volume.plane_dimensions(self.mpr_plane);
                let rgba = scalar_to_rgba(&pixels, self.window_center, self.window_width);
                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                self.texture = Some(ctx.load_texture(
                    "dicom_image",
                    color_image,
                    egui::TextureOptions::LINEAR,
                ));
                self.displayed_physical_size = Some(volume.physical_size(self.mpr_plane));
            }
        }

        self.wl_dirty = false;
    }
}

impl eframe::App for DicomViewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending file load (first frame, or after Open dialog)
        if let Some(paths) = self.pending_load.take() {
            self.load_files(paths, ctx);
        }

        // ── Poll background loading thread ─────────────────────────────────
        let mut loading_complete = false;
        if let Some(state) = &mut self.loading {
            loop {
                match state.rx.try_recv() {
                    Ok(LoadMsg::Image(image)) => {
                        state.received += 1;
                        state.current_filename = image.filename.clone();
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

        // Rebuild texture when window/level values change
        if self.wl_dirty {
            self.update_current_slice_view(ctx);
        }

        // Drag-and-drop: detect hovered and dropped files
        ctx.input(|i| {
            self.files_hovered = !i.raw.hovered_files.is_empty();
        });

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        if !dropped.is_empty() {
            self.files_hovered = false;
            self.load_files(dropped, ctx);
        }

        // Keyboard navigation: arrow keys
        ctx.input(|i| {
            if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowLeft) {
                let current = match self.view_mode {
                    ViewMode::Stack => self.current_stack_slice,
                    ViewMode::Mpr => self.current_mpr_slice(),
                };
                if current > 0 {
                    match self.view_mode {
                        ViewMode::Stack => self.current_stack_slice -= 1,
                        ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                    }
                    self.wl_dirty = true;
                }
            }
            if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight) {
                let len = self.current_view_slice_len();
                let current = match self.view_mode {
                    ViewMode::Stack => self.current_stack_slice,
                    ViewMode::Mpr => self.current_mpr_slice(),
                };
                if current < len.saturating_sub(1) {
                    match self.view_mode {
                        ViewMode::Stack => self.current_stack_slice += 1,
                        ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                    }
                    self.wl_dirty = true;
                }
            }
        });

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📄 Open Files").clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter("DICOM", &["dcm", "DCM"])
                        .add_filter("DICOM", &["dicom", "DICOM"])
                        .add_filter("All Files", &["*"])
                        .pick_files()
                    {
                        self.pending_load = Some(paths);
                    }
                }

                if ui.button("📁 Open Folder").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.pending_load = Some(vec![folder]);
                    }
                }

                ui.separator();
                ui.toggle_value(&mut self.show_metadata, "ℹ Metadata");

                if !self.series_groups.is_empty() {
                    ui.separator();
                    ui.label("Series:");
                    let selected_text = self
                        .series_groups
                        .get(self.current_series)
                        .map(|g| g.label.as_str())
                        .unwrap_or("(none)");
                    egui::ComboBox::from_id_salt("series_selector")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            let mut selected_series = self.current_series;
                            for (idx, group) in self.series_groups.iter().enumerate() {
                                ui.selectable_value(
                                    &mut selected_series,
                                    idx,
                                    format!("{} ({} images)", group.label, group.image_indices.len()),
                                );
                            }
                            if selected_series != self.current_series {
                                self.current_series = selected_series;
                                self.current_stack_slice = 0;
                                self.current_axial_slice = 0;
                                self.current_coronal_slice = 0;
                                self.current_sagittal_slice = 0;
                                self.pan = Vec2::ZERO;
                                self.refresh_mpr_volume();
                                self.apply_default_window_for_current_series();
                                self.wl_dirty = true;
                            }
                        });

                    ui.separator();
                    ui.label("View:");
                    let previous_view_mode = self.view_mode;
                    ui.selectable_value(&mut self.view_mode, ViewMode::Stack, "Stack");
                    ui.add_enabled_ui(self.mpr_available(), |ui| {
                        ui.selectable_value(&mut self.view_mode, ViewMode::Mpr, "MPR");
                    });
                    if !self.mpr_available() {
                        let reason = self
                            .mpr_error
                            .as_deref()
                            .unwrap_or("MPR unavailable for this series");
                        ui.label(egui::RichText::new(reason).small().weak());
                    }

                    if self.view_mode == ViewMode::Mpr && previous_view_mode != ViewMode::Mpr {
                        self.apply_default_window_for_current_series();
                        self.wl_dirty = true;
                    } else if self.view_mode == ViewMode::Stack && previous_view_mode != ViewMode::Stack {
                        self.apply_default_window_for_current_series();
                        self.wl_dirty = true;
                    }

                    if self.view_mode == ViewMode::Mpr {
                        ui.separator();
                        ui.label("Plane:");
                        let previous_plane = self.mpr_plane;
                        ui.selectable_value(&mut self.mpr_plane, MprPlane::Axial, "Axial");
                        ui.selectable_value(&mut self.mpr_plane, MprPlane::Coronal, "Coronal");
                        ui.selectable_value(&mut self.mpr_plane, MprPlane::Sagittal, "Sagittal");
                        if previous_plane != self.mpr_plane {
                            self.wl_dirty = true;
                        }
                    }
                }

                ui.separator();
                if ui.button("Fit").clicked() {
                    self.zoom = 1.0;
                    self.rotation_degrees = 0.0;
                    self.pan = Vec2::ZERO;
                }
                if ui.button("+").clicked() {
                    self.zoom = (self.zoom * 1.25).min(20.0);
                }
                if ui.button("–").clicked() {
                    self.zoom = (self.zoom / 1.25).max(0.05);
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.button("⟲").on_hover_text("Rotate 90° counter-clockwise").clicked() {
                    self.rotation_degrees = (self.rotation_degrees - 90.0).rem_euclid(360.0);
                }
                if ui.button("⟳").on_hover_text("Rotate 90° clockwise").clicked() {
                    self.rotation_degrees = (self.rotation_degrees + 90.0).rem_euclid(360.0);
                }
                ui.label(format!("{:.0}°", self.rotation_degrees));
                ui.small("(Mouse4 drag to rotate)");

                // Window/Level controls (only for 16-bit grayscale)
                let windowing_enabled = match self.view_mode {
                    ViewMode::Stack => self
                        .active_image()
                        .map(|image| image.raw_image.is_grayscale16())
                        .unwrap_or(false),
                    ViewMode::Mpr => self.mpr_volume.is_some(),
                };
                if windowing_enabled {
                    ui.separator();
                    ui.label("WC:");
                    let wc_resp = ui.add(
                        egui::DragValue::new(&mut self.window_center).speed(1.0),
                    );
                    ui.label("WW:");
                    let ww_resp = ui.add(
                        egui::DragValue::new(&mut self.window_width)
                            .speed(1.0)
                            .range(1.0..=f32::MAX),
                    );
                    if wc_resp.changed() || ww_resp.changed() {
                        self.wl_dirty = true;
                    }
                }

                // Slice navigation
                let active_len = self.current_view_slice_len();
                if active_len > 1 {
                    ui.separator();
                    let current_index = match self.view_mode {
                        ViewMode::Stack => self.current_stack_slice,
                        ViewMode::Mpr => self.current_mpr_slice(),
                    };
                    let label = match self.view_mode {
                        ViewMode::Stack => "Slice".to_string(),
                        ViewMode::Mpr => format!("{}", self.mpr_plane.label()),
                    };
                    ui.label(format!("{}: {}/{}", label, current_index + 1, active_len));
                    let mut slice_val = current_index as i32;
                    let slider = ui.add(
                        egui::Slider::new(&mut slice_val, 0..=(active_len - 1) as i32)
                            .show_value(false)
                    );
                    if slider.changed() {
                        match self.view_mode {
                            ViewMode::Stack => self.current_stack_slice = slice_val as usize,
                            ViewMode::Mpr => self.set_current_mpr_slice(slice_val as usize),
                        }
                        self.wl_dirty = true;
                    }
                }
            });
        });

        // ── Metadata side panel ───────────────────────────────────────────────
        if self.show_metadata && self.active_series_len() > 0 {
            let Some(active_image) = self.metadata_image() else {
                return;
            };
            let metadata = &active_image.metadata;
            if !metadata.is_empty() {
                egui::SidePanel::right("metadata_panel")
                    .min_width(180.0)
                    .max_width(300.0)
                    .show(ctx, |ui| {
                        ui.heading("Metadata");
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            egui::Grid::new("meta_grid")
                                .num_columns(2)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (label, value) in metadata {
                                        ui.strong(label);
                                        ui.label(value);
                                        ui.end_row();
                                    }
                                });
                        });
                    });
            }
        }

        // ── Image panel ───────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
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

            if self.texture.is_none() {
                ui.centered_and_justified(|ui| {
                    if self.view_mode == ViewMode::Mpr {
                        ui.label(
                            self.mpr_error
                                .as_deref()
                                .unwrap_or("MPR is not available for the selected series"),
                        );
                    } else {
                        ui.label("Open DICOM files/folders or drag and drop them here");
                    }
                });
                return;
            }

            if let Some(texture) = self.texture.clone() {
                let rect = ui.available_rect_before_wrap();
                let tex_size = texture.size();
                let img_w = tex_size[0] as f32;
                let img_h = tex_size[1] as f32;
                let physical_size = self
                    .displayed_physical_size
                    .unwrap_or_else(|| egui::vec2(img_w, img_h));

                // Scale to fit the panel at current zoom level
                let fit = (rect.width() / physical_size.x).min(rect.height() / physical_size.y);
                let display = egui::vec2(
                    physical_size.x * fit * self.zoom,
                    physical_size.y * fit * self.zoom,
                );

                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // Check button states for multi-button combinations
                let (left_down, right_down, middle_down, side1_down) = ctx.input(|i| {
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
                let scroll_delta = ctx.input(|i| i.smooth_scroll_delta.y);
                if response.hovered() && scroll_delta != 0.0 {
                    if self.current_view_slice_len() > 1 {
                        // Scroll to change slice
                        let current = match self.view_mode {
                            ViewMode::Stack => self.current_stack_slice,
                            ViewMode::Mpr => self.current_mpr_slice(),
                        };
                        if scroll_delta > 0.0 && current > 0 {
                            match self.view_mode {
                                ViewMode::Stack => self.current_stack_slice -= 1,
                                ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                            }
                            self.wl_dirty = true;
                        } else if scroll_delta < 0.0 && current < self.current_view_slice_len() - 1 {
                            match self.view_mode {
                                ViewMode::Stack => self.current_stack_slice += 1,
                                ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                            }
                            self.wl_dirty = true;
                        }
                    } else {
                        // Scroll to zoom
                        self.zoom =
                            (self.zoom * (1.0 + scroll_delta * 0.004)).clamp(0.05, 20.0);
                    }
                }

                // Both left and right buttons → zoom
                if response.hovered() && left_down && right_down {
                    let delta = response.drag_delta();
                    // Downward drag zooms in, upward zooms out
                    if delta.y != 0.0 {
                        self.zoom = (self.zoom * (1.0 + delta.y * 0.01)).clamp(0.05, 20.0);
                    }
                }
                // Middle mouse button → scroll through slices (if multiple images)
                else if response.hovered() && middle_down && self.current_view_slice_len() > 1 {
                    let delta = response.drag_delta();
                    let current = match self.view_mode {
                        ViewMode::Stack => self.current_stack_slice,
                        ViewMode::Mpr => self.current_mpr_slice(),
                    };
                    // Upward drag → previous slice, downward → next slice
                    if delta.y > 2.0 && current > 0 {
                        match self.view_mode {
                            ViewMode::Stack => self.current_stack_slice -= 1,
                            ViewMode::Mpr => self.set_current_mpr_slice(current - 1),
                        }
                        self.wl_dirty = true;
                    } else if delta.y < -2.0 && current < self.current_view_slice_len() - 1 {
                        match self.view_mode {
                            ViewMode::Stack => self.current_stack_slice += 1,
                            ViewMode::Mpr => self.set_current_mpr_slice(current + 1),
                        }
                        self.wl_dirty = true;
                    }
                }
                // Side mouse button drag (Mouse4) -> rotate image
                else if response.hovered() && side1_down {
                    let delta = ctx.input(|i| i.pointer.delta());
                    if delta.x != 0.0 || delta.y != 0.0 {
                        // Use both axes so diagonal/vertical drag can rotate too.
                        let rotation_delta = delta.x * 0.35 + delta.y * 0.35;
                        self.rotation_degrees =
                            (self.rotation_degrees + rotation_delta).rem_euclid(360.0);
                    }
                }
                // Left-button drag → pan (only if right button is not pressed)
                else if response.dragged_by(egui::PointerButton::Primary) && !right_down {
                    self.pan += response.drag_delta();
                }
                // Right-button drag → window / level (only if left button is not pressed)
                // Horizontal drag adjusts Window Width; vertical drag adjusts Window Centre.
                // Only meaningful for 16-bit grayscale; ignored otherwise.
                else if response.dragged_by(egui::PointerButton::Secondary) && !left_down {
                    let windowing_enabled = match self.view_mode {
                        ViewMode::Stack => self
                            .active_image()
                            .map(|image| image.raw_image.is_grayscale16())
                            .unwrap_or(false),
                        ViewMode::Mpr => self.mpr_volume.is_some(),
                    };
                    if windowing_enabled {
                        let delta = response.drag_delta();
                        let ww_scale = 2.0_f32;
                        let wc_scale = 2.0_f32;
                        self.window_width = (self.window_width + delta.x * ww_scale).max(1.0);
                        self.window_center += -delta.y * wc_scale;
                        self.wl_dirty = true;
                    }
                }

                // Double-click → reset view
                if response.double_clicked() {
                    self.zoom = 1.0;
                    self.rotation_degrees = 0.0;
                    self.pan = Vec2::ZERO;
                    self.apply_default_window_for_current_series();
                    self.wl_dirty = true;
                }

                let center = rect.center() + self.pan;
                let angle_rad = self.rotation_degrees.to_radians();
                paint_rotated_texture(ui.painter(), texture.id(), center, display, angle_rad);
            }
        });
        // ── Loading progress modal ────────────────────────────────────────────
        if let Some(state) = &self.loading {
            let total = state.total;
            let received = state.received;
            let filename = state.current_filename.clone();

            // Dim the background
            let screen = ctx.viewport_rect();
            egui::Area::new(egui::Id::new("loading_overlay"))
                .fixed_pos(screen.min)
                .order(egui::Order::Background)
                .show(ctx, |ui| {
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
                .show(ctx, |ui| {
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
    }
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
        assert_eq!(
            volume.extract_plane(MprPlane::Coronal, 1),
            vec![4.0, 5.0, 6.0, 10.0, 11.0, 12.0]
        );
        assert_eq!(
            volume.extract_plane(MprPlane::Sagittal, 2),
            vec![3.0, 6.0, 9.0, 12.0]
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
            pixel_spacing: Some([1.0, 1.0]),
            slice_thickness: Some(1.0),
            spacing_between_slices: Some(1.0),
            image_position_patient: Some([0.0, 0.0, 0.0]),
            image_orientation_patient: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        }];

        let err = MprVolume::from_images(&images, &[0]).expect_err("rgb series should fail");
        assert!(err.contains("at least 2 slices") || err.contains("grayscale"));
    }
}

pub fn run_viewer() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DICOM Viewer")
            .with_inner_size([1024.0, 768.0]),
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
        ..Default::default()
    };
    let path_bufs = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    eframe::run_native(
        "DICOM Viewer",
        options,
        Box::new(move |_cc| {
            let mut app = DicomViewApp::new(None);
            app.pending_load = Some(path_bufs);
            Ok(Box::new(app))
        }),
    )
    .expect("failed to launch DICOM Viewer");
}
