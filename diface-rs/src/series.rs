//! DICOM series discovery, loading and defaced writing.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use dicom_core::value::PrimitiveValue;
use dicom_core::VR;
use dicom_object::mem::InMemElement;
use dicom_object::{open_file, DefaultDicomObject, Tag};
use dicom_pixeldata::{ConvertOptions, ModalityLutOption, PixelDecoder, PixelRepresentation};

use crate::geometry::{cross, dot, normalize, V3};
use crate::volume::{Mask, Volume};

/// Transport syntax written for defaced output.
pub const EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1";

/// One source slice of a series.
#[derive(Clone, Debug)]
pub struct SliceEntry {
    pub path: PathBuf,
    pub sop_instance_uid: String,
    pub instance_number: i32,
    pub position: V3,
    /// Projection of `position` onto the slice normal (sort key).
    pub sort_position: f64,
    pub rescale_slope: f32,
    pub rescale_intercept: f32,
    pub bits_allocated: u16,
    pub signed: bool,
    pub rows: usize,
    pub cols: usize,
}

/// A group of files sharing a Series Instance UID.
#[derive(Clone, Debug)]
pub struct SeriesGroup {
    pub uid: String,
    pub files: Vec<PathBuf>,
}

fn has_dicom_preamble(path: &Path) -> bool {
    let mut header = [0u8; 132];
    match fs::File::open(path).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => &header[128..132] == b"DICM",
        Err(_) => false,
    }
}

fn is_candidate_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let e = ext.to_ascii_lowercase();
            e == "dcm" || e == "dicom" || e == "ima"
        }
        None => has_dicom_preamble(path),
    }
}

/// Collect candidate DICOM files from a file or directory root.
pub fn collect_dicom_files(root: &Path, recursive: bool) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(format!("input path does not exist: {}", root.display()));
    }

    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("failed to read {}: {}", dir.display(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    stack.push(path);
                }
            } else if is_candidate_file(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Read the Series Instance UID from a DICOM file.
fn read_series_uid(obj: &DefaultDicomObject) -> Option<String> {
    obj.element(Tag(0x0020, 0x000E))
        .ok()
        .and_then(|e| e.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Group files by Series Instance UID, preserving discovery order.
pub fn group_by_series(files: &[PathBuf]) -> Result<Vec<SeriesGroup>, String> {
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut ungrouped = 0usize;
    for path in files {
        let obj = match open_file(path) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let key = read_series_uid(&obj).unwrap_or_else(|| {
            ungrouped += 1;
            format!("UNGROUPED-{:04}", ungrouped)
        });
        groups.entry(key).or_default().push(path.clone());
    }
    Ok(groups
        .into_iter()
        .map(|(uid, files)| SeriesGroup { uid, files })
        .collect())
}

fn get_str(obj: &DefaultDicomObject, tag: Tag) -> Option<String> {
    obj.element(tag)
        .ok()?
        .to_str()
        .ok()
        .map(|s| s.trim().to_string())
}

fn get_f32(obj: &DefaultDicomObject, tag: Tag) -> Option<f32> {
    get_str(obj, tag).and_then(|s| {
        s.split('\\')
            .next()
            .and_then(|v| v.trim().parse::<f32>().ok())
    })
}

fn get_i32(obj: &DefaultDicomObject, tag: Tag) -> Option<i32> {
    get_str(obj, tag).and_then(|s| {
        s.split('\\')
            .next()
            .and_then(|v| v.trim().parse::<i32>().ok())
    })
}

fn get_multi_f32(obj: &DefaultDicomObject, tag: Tag) -> Option<Vec<f32>> {
    let s = get_str(obj, tag)?;
    let values: Vec<f32> = s
        .split('\\')
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .collect();
    (!values.is_empty()).then_some(values)
}

struct ParsedSlice {
    entry: SliceEntry,
    raw: Vec<f32>,
    image_orientation: [f32; 6],
}

/// Parse and pixel-decode a single file.
fn parse_slice(path: &Path) -> Result<ParsedSlice, String> {
    let obj = open_file(path).map_err(|e| format!("failed to open {}: {}", path.display(), e))?;

    let iop = get_multi_f32(&obj, Tag(0x0020, 0x0037))
        .filter(|v| v.len() >= 6)
        .ok_or_else(|| format!("{} is missing ImageOrientationPatient", path.display()))?;
    let iop: [f32; 6] = [iop[0], iop[1], iop[2], iop[3], iop[4], iop[5]];

    let ipp = get_multi_f32(&obj, Tag(0x0020, 0x0032))
        .filter(|v| v.len() >= 3)
        .ok_or_else(|| format!("{} is missing ImagePositionPatient", path.display()))?;
    let position: V3 = [ipp[0] as f64, ipp[1] as f64, ipp[2] as f64];

    let decoded = obj
        .decode_pixel_data()
        .map_err(|e| format!("failed to decode pixels in {}: {}", path.display(), e))?;

    if decoded.samples_per_pixel() != 1 {
        return Err(format!(
            "{} has {} samples per pixel; only grayscale series are supported",
            path.display(),
            decoded.samples_per_pixel()
        ));
    }
    if decoded.number_of_frames() != 1 {
        return Err(format!(
            "{} is multi-frame ({} frames); multi-file single-frame series are expected",
            path.display(),
            decoded.number_of_frames()
        ));
    }

    let rows = decoded.rows() as usize;
    let cols = decoded.columns() as usize;
    let bits_allocated = decoded.bits_allocated();
    let signed = matches!(decoded.pixel_representation(), PixelRepresentation::Signed);

    let opts = ConvertOptions::new().with_modality_lut(ModalityLutOption::None);
    let raw: Vec<f32> = decoded
        .to_vec_with_options::<f32>(&opts)
        .map_err(|e| format!("failed to convert pixels in {}: {}", path.display(), e))?;
    if raw.len() != rows * cols {
        return Err(format!(
            "{} pixel buffer length {} does not match {}x{}",
            path.display(),
            raw.len(),
            rows,
            cols
        ));
    }

    let entry = SliceEntry {
        path: path.to_path_buf(),
        sop_instance_uid: get_str(&obj, Tag(0x0008, 0x0018)).unwrap_or_default(),
        instance_number: get_i32(&obj, Tag(0x0020, 0x0013)).unwrap_or(0),
        position,
        sort_position: 0.0,
        rescale_slope: get_f32(&obj, Tag(0x0028, 0x1053)).unwrap_or(1.0),
        rescale_intercept: get_f32(&obj, Tag(0x0028, 0x1052)).unwrap_or(0.0),
        bits_allocated,
        signed,
        rows,
        cols,
    };

    Ok(ParsedSlice {
        entry,
        raw,
        image_orientation: iop,
    })
}

/// Load a list of files as a single 3D volume.
///
/// Returns the volume together with its slices sorted by position.
pub fn load_series(files: &[PathBuf]) -> Result<(Volume, Vec<SliceEntry>), String> {
    if files.is_empty() {
        return Err("no files supplied for series".to_string());
    }

    let mut parsed: Vec<ParsedSlice> = Vec::with_capacity(files.len());
    for path in files {
        parsed.push(parse_slice(path)?);
    }

    // All slices must share dimensions. Orientation, however, may vary between
    // slices (e.g. curved reformats, or acquisitions where a few localisers were
    // saved under the same SeriesInstanceUID). Such a series is not a rigid
    // volume, so instead of rejecting the whole thing the slices are split into
    // consistent orientation groups and the largest group is defaced.
    let (rows, cols) = (parsed[0].entry.rows, parsed[0].entry.cols);
    let (pixel_spacing, spacing_between_slices, slice_thickness, modality) = {
        let obj = open_file(&parsed[0].entry.path).map_err(|e| e.to_string())?;
        let ps = get_multi_f32(&obj, Tag(0x0028, 0x0030))
            .map(|v| [v[0], v.get(1).copied().unwrap_or(v[0])])
            .unwrap_or([1.0, 1.0]);
        (
            ps,
            get_f32(&obj, Tag(0x0018, 0x0088)),
            get_f32(&obj, Tag(0x0018, 0x0050)),
            get_str(&obj, Tag(0x0008, 0x0060)).unwrap_or_default(),
        )
    };

    for slice in &parsed {
        if slice.entry.rows != rows || slice.entry.cols != cols {
            return Err(format!(
                "{} dimensions {}x{} differ from {}x{}",
                slice.entry.path.display(),
                slice.entry.cols,
                slice.entry.rows,
                cols,
                rows
            ));
        }
    }

    // Group slices by orientation and keep the largest consistent group.
    let mut groups: Vec<(usize, Vec<ParsedSlice>)> = Vec::new();
    for slice in parsed {
        let matching = groups.iter_mut().find(|(_, members)| {
            let iop = members[0].image_orientation;
            (0..6).all(|i| (slice.image_orientation[i] - iop[i]).abs() <= 1e-3)
        });
        match matching {
            Some((_, members)) => members.push(slice),
            None => groups.push((0, vec![slice])),
        }
    }
    for (order, (_, members)) in groups.iter_mut().enumerate() {
        let _ = order;
        members.sort_by(|a, b| {
            a.entry
                .instance_number
                .cmp(&b.entry.instance_number)
                .then_with(|| a.entry.path.cmp(&b.entry.path))
        });
    }
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut parsed = groups
        .into_iter()
        .next()
        .map(|(_, members)| members)
        .unwrap_or_default();
    if parsed.is_empty() {
        return Err("no slices with a usable orientation".to_string());
    }

    let reference_iop = parsed[0].image_orientation;
    let ref_row = normalize([
        reference_iop[0] as f64,
        reference_iop[1] as f64,
        reference_iop[2] as f64,
    ])
    .ok_or_else(|| "invalid ImageOrientationPatient row direction".to_string())?;
    let ref_col = normalize([
        reference_iop[3] as f64,
        reference_iop[4] as f64,
        reference_iop[5] as f64,
    ])
    .ok_or_else(|| "invalid ImageOrientationPatient column direction".to_string())?;
    let normal = normalize(cross(ref_row, ref_col))
        .ok_or_else(|| "ImageOrientationPatient vectors are parallel".to_string())?;

    for slice in &mut parsed {
        slice.entry.sort_position = dot(slice.entry.position, normal);
    }

    parsed.sort_by(|a, b| {
        a.entry
            .sort_position
            .partial_cmp(&b.entry.sort_position)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entry.instance_number.cmp(&b.entry.instance_number))
    });

    // Slice spacing from positions, falling back to DICOM spacing tags.
    let slice_spacing = {
        let mut deltas = Vec::new();
        for pair in parsed.windows(2) {
            let d = (pair[1].entry.sort_position - pair[0].entry.sort_position).abs();
            if d > 1e-3 {
                deltas.push(d);
            }
        }
        if !deltas.is_empty() {
            deltas.iter().sum::<f64>() / deltas.len() as f64
        } else {
            spacing_between_slices
                .or(slice_thickness)
                .map(|v| v.abs() as f64)
                .unwrap_or(1.0)
                .max(0.001)
        }
    };

    let dims = [cols, rows, parsed.len()];
    let nxy = cols * rows;
    let mut data: Vec<f32> = Vec::with_capacity(nxy * parsed.len());
    let mut rescale: Vec<(f32, f32)> = Vec::with_capacity(parsed.len());
    let mut entries: Vec<SliceEntry> = Vec::with_capacity(parsed.len());

    for slice in parsed {
        data.extend_from_slice(&slice.raw);
        rescale.push((slice.entry.rescale_slope, slice.entry.rescale_intercept));
        entries.push(slice.entry);
    }

    let volume = Volume {
        data,
        dims,
        // PixelSpacing = [row spacing, column spacing]; x = column, y = row.
        spacing: [
            pixel_spacing[1].abs().max(0.001) as f64,
            pixel_spacing[0].abs().max(0.001) as f64,
            slice_spacing,
        ],
        origin: entries[0].position,
        dir: [ref_row, ref_col, normal],
        rescale,
        modality,
    };

    Ok((volume, entries))
}

/// Build a native-typed primitive value matching the source pixel encoding.
fn native_primitive(bits_allocated: u16, signed: bool, values: &[f32]) -> PrimitiveValue {
    match (bits_allocated, signed) {
        (8, false) => {
            let v: Vec<u8> = values
                .iter()
                .map(|x| x.round().clamp(0.0, 255.0) as u8)
                .collect();
            PrimitiveValue::U8(v.into())
        }
        (8, true) => {
            // DICOM has no I8 primitive variant; store the two's-complement byte.
            let v: Vec<u8> = values
                .iter()
                .map(|x| (x.round().clamp(-128.0, 127.0) as i8) as u8)
                .collect();
            PrimitiveValue::U8(v.into())
        }
        (b, true) if b <= 16 => {
            let v: Vec<i16> = values
                .iter()
                .map(|x| x.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16)
                .collect();
            PrimitiveValue::I16(v.into())
        }
        (b, false) if b <= 16 => {
            let v: Vec<u16> = values
                .iter()
                .map(|x| x.round().clamp(0.0, u16::MAX as f32) as u16)
                .collect();
            PrimitiveValue::U16(v.into())
        }
        (_, true) => {
            let v: Vec<i32> = values.iter().map(|x| x.round() as i32).collect();
            PrimitiveValue::I32(v.into())
        }
        (_, false) => {
            let v: Vec<u32> = values
                .iter()
                .map(|x| x.round().clamp(0.0, u32::MAX as f32) as u32)
                .collect();
            PrimitiveValue::U32(v.into())
        }
    }
}

/// Write a defaced series to `out_dir`, one output file per input slice.
///
/// `fill_raw` is the raw stored value written into removed voxels.
pub fn write_defaced_series(
    entries: &[SliceEntry],
    volume: &Volume,
    mask: &Mask,
    fill_raw: f32,
    out_dir: &Path,
) -> Result<usize, String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create {}: {}", out_dir.display(), e))?;

    let nx = volume.nx();
    let ny = volume.ny();
    let nxy = nx * ny;

    let mut written = 0usize;
    for (z, entry) in entries.iter().enumerate() {
        let mut obj = open_file(&entry.path)
            .map_err(|e| format!("failed to reopen {}: {}", entry.path.display(), e))?;

        let base = z * nxy;
        let mut slice: Vec<f32> = Vec::with_capacity(nxy);
        for i in 0..nxy {
            let orig = volume.data[base + i];
            let v = if mask.remove[base + i] != 0 {
                // Feathered masks blend toward the fill by `weight`; a binary
                // mask (no weight buffer) is fully replaced.
                let w = mask.weight_at(base + i);
                orig + (fill_raw - orig) * w
            } else {
                orig
            };
            slice.push(v);
        }

        let vr = if entry.bits_allocated > 8 {
            VR::OW
        } else {
            VR::OB
        };
        let primitive = native_primitive(entry.bits_allocated, entry.signed, &slice);
        let elem: InMemElement = InMemElement::new(Tag(0x7FE0, 0x0010), vr, primitive);
        obj.put(elem);

        // Ensure the (now native) pixel data is written as explicit VR little endian.
        obj.meta_mut().transfer_syntax = EXPLICIT_VR_LITTLE_ENDIAN.to_string();

        let name = entry
            .path
            .file_name()
            .ok_or_else(|| "input file has no name".to_string())?;
        let out_path = out_dir.join(name);
        obj.write_to_file(&out_path)
            .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
        written += 1;
    }

    Ok(written)
}
