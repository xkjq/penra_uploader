use eframe::egui;
use egui::Vec2;
use dicom_object::{open_file, Tag};
use std::path::PathBuf;

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
            RawImage::Gray16(g) => {
                let n = g.width * g.height;
                let mut rgba = vec![0u8; n * 4];
                let lo = wc - ww / 2.0;
                let hi = wc + ww / 2.0;
                for (i, &v) in g.data.iter().enumerate() {
                    let norm = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                    let byte = (norm * 255.0) as u8;
                    rgba[i * 4] = byte;
                    rgba[i * 4 + 1] = byte;
                    rgba[i * 4 + 2] = byte;
                    rgba[i * 4 + 3] = 255;
                }
                rgba
            }
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

struct DicomViewApp {
    pending_load: Option<PathBuf>,
    current_file: Option<PathBuf>,
    raw_image: Option<RawImage>,
    texture: Option<egui::TextureHandle>,
    metadata: Vec<(String, String)>,
    zoom: f32,
    pan: Vec2,
    error: Option<String>,
    show_metadata: bool,
    window_center: f32,
    window_width: f32,
    wl_dirty: bool,
}

impl DicomViewApp {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            pending_load: path,
            current_file: None,
            raw_image: None,
            texture: None,
            metadata: Vec::new(),
            zoom: 1.0,
            pan: Vec2::ZERO,
            error: None,
            show_metadata: true,
            window_center: 0.0,
            window_width: 1.0,
            wl_dirty: false,
        }
    }

    fn load_file(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.error = None;
        self.metadata.clear();
        self.texture = None;
        self.raw_image = None;
        self.pan = Vec2::ZERO;
        self.zoom = 1.0;
        self.wl_dirty = false;

        let obj = match open_file(&path) {
            Ok(o) => o,
            Err(e) => {
                self.error = Some(format!("Failed to open file: {}", e));
                return;
            }
        };

        // Collect display metadata (text tags)
        for (tag, label) in METADATA_TAGS {
            if let Ok(elem) = obj.element(*tag) {
                if let Ok(val) = elem.to_str() {
                    let val = val.trim().to_string();
                    if !val.is_empty() {
                        self.metadata.push((label.to_string(), val));
                    }
                }
            }
        }

        // Helper: read a tag as string then parse
        let get_str = |tag: Tag| -> Option<String> {
            obj.element(tag)
                .ok()?
                .to_str()
                .ok()
                .map(|s| s.trim().to_string())
        };
        let get_u16 = |tag: Tag| -> Option<u16> {
            get_str(tag).and_then(|s| s.parse::<u16>().ok())
        };
        let get_f32 = |tag: Tag| -> Option<f32> {
            // Window Center/Width can be multi-valued (backslash-separated); take first
            get_str(tag).and_then(|s| {
                s.split('\\').next().and_then(|v| v.trim().parse::<f32>().ok())
            })
        };

        let rows = match get_u16(Tag(0x0028, 0x0010)) {
            Some(v) => v as usize,
            None => {
                self.error = Some("Missing or unreadable Rows tag".to_string());
                return;
            }
        };
        let cols = match get_u16(Tag(0x0028, 0x0011)) {
            Some(v) => v as usize,
            None => {
                self.error = Some("Missing or unreadable Columns tag".to_string());
                return;
            }
        };

        let bits = get_u16(Tag(0x0028, 0x0100)).unwrap_or(8);
        let samples = get_u16(Tag(0x0028, 0x0002)).unwrap_or(1);
        let signed = get_u16(Tag(0x0028, 0x0103)).unwrap_or(0) == 1;
        let rescale_intercept = get_f32(Tag(0x0028, 0x1052)).unwrap_or(0.0);
        let rescale_slope = get_f32(Tag(0x0028, 0x1053)).unwrap_or(1.0);
        let wc_dicom = get_f32(Tag(0x0028, 0x1050));
        let ww_dicom = get_f32(Tag(0x0028, 0x1051));

        // Read raw pixel bytes (fails for compressed transfer syntaxes)
        let pixel_elem = match obj.element(Tag(0x7FE0, 0x0010)) {
            Ok(e) => e,
            Err(_) => {
                self.error = Some("No pixel data found in this file".to_string());
                return;
            }
        };
        let raw_bytes = match pixel_elem.to_bytes() {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(format!(
                    "Cannot read pixel data (compressed format not supported): {}",
                    e
                ));
                return;
            }
        };
        let bytes: &[u8] = &raw_bytes;

        // Decode to RawImage based on bit depth and sample count
        let raw_image = match (bits, samples) {
            (8, 1) => {
                if bytes.len() < rows * cols {
                    self.error = Some("Pixel data buffer is too short".to_string());
                    return;
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
                    self.error = Some(format!(
                        "Pixel data buffer too short: {} bytes, expected {}",
                        bytes.len(),
                        expected
                    ));
                    return;
                }
                // Parse 16-bit little-endian values and apply rescale
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
                    self.error = Some("Pixel data buffer is too short".to_string());
                    return;
                }
                RawImage::Rgb8 {
                    data: bytes[..expected].to_vec(),
                    width: cols,
                    height: rows,
                }
            }
            _ => {
                self.error = Some(format!(
                    "Unsupported pixel format: {} bpp, {} samples per pixel",
                    bits, samples
                ));
                return;
            }
        };

        // Determine initial window centre/width for 16-bit grayscale
        if let RawImage::Gray16(ref g) = raw_image {
            let (wc, ww) = if let (Some(wc), Some(ww)) = (wc_dicom, ww_dicom) {
                (wc, ww)
            } else {
                // Compute from the actual pixel values
                let min = g.data.iter().cloned().fold(f32::MAX, f32::min);
                let max = g.data.iter().cloned().fold(f32::MIN, f32::max);
                ((min + max) / 2.0, (max - min).max(1.0))
            };
            self.window_center = wc;
            self.window_width = ww;
        }

        self.raw_image = Some(raw_image);
        self.current_file = Some(path);
        self.rebuild_texture(ctx);
    }

    fn rebuild_texture(&mut self, ctx: &egui::Context) {
        let raw = match &self.raw_image {
            Some(r) => r,
            None => return,
        };
        let (width, height) = raw.dimensions();
        let rgba = raw.to_rgba(self.window_center, self.window_width);
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
        self.texture = Some(ctx.load_texture(
            "dicom_image",
            color_image,
            egui::TextureOptions::LINEAR,
        ));
        self.wl_dirty = false;
    }
}

impl eframe::App for DicomViewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending file load (first frame, or after Open dialog)
        if let Some(path) = self.pending_load.take() {
            self.load_file(ctx, path);
        }

        // Rebuild texture when window/level values change
        if self.wl_dirty {
            self.rebuild_texture(ctx);
        }

        // Drag-and-drop: accept the first dropped file
        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.load_file(ctx, path);
        }

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("DICOM", &["dcm", "DCM"])
                        .add_filter("All Files", &["*"])
                        .pick_file()
                    {
                        self.pending_load = Some(path);
                    }
                }

                ui.separator();
                ui.toggle_value(&mut self.show_metadata, "ℹ Metadata");

                ui.separator();
                if ui.button("Fit").clicked() {
                    self.zoom = 1.0;
                    self.pan = Vec2::ZERO;
                }
                if ui.button("+").clicked() {
                    self.zoom = (self.zoom * 1.25).min(20.0);
                }
                if ui.button("–").clicked() {
                    self.zoom = (self.zoom / 1.25).max(0.05);
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));

                // Window/Level controls (only for 16-bit grayscale)
                if self.raw_image.as_ref().map(|r| r.is_grayscale16()).unwrap_or(false) {
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

                if let Some(p) = &self.current_file {
                    ui.separator();
                    ui.label(p.file_name().and_then(|n| n.to_str()).unwrap_or(""));
                }
            });
        });

        // ── Metadata side panel ───────────────────────────────────────────────
        if self.show_metadata && !self.metadata.is_empty() {
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
                                for (label, value) in &self.metadata {
                                    ui.strong(label);
                                    ui.label(value);
                                    ui.end_row();
                                }
                            });
                    });
                });
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

            if self.texture.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a DICOM file or drag and drop one here");
                });
                return;
            }

            if let Some(texture) = &self.texture {
                let rect = ui.available_rect_before_wrap();
                let tex_size = texture.size();
                let img_w = tex_size[0] as f32;
                let img_h = tex_size[1] as f32;

                // Scale to fit the panel at current zoom level
                let fit = (rect.width() / img_w).min(rect.height() / img_h);
                let display = egui::vec2(img_w * fit * self.zoom, img_h * fit * self.zoom);

                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // Scroll wheel → zoom
                let scroll_delta = ctx.input(|i| i.smooth_scroll_delta.y);
                if response.hovered() && scroll_delta != 0.0 {
                    self.zoom =
                        (self.zoom * (1.0 + scroll_delta * 0.004)).clamp(0.05, 20.0);
                }

                // Left-button drag → pan
                if response.dragged_by(egui::PointerButton::Primary) {
                    self.pan += response.drag_delta();
                }

                // Double-click → reset view
                if response.double_clicked() {
                    self.zoom = 1.0;
                    self.pan = Vec2::ZERO;
                }

                let center = rect.center() + self.pan;
                let image_rect = egui::Rect::from_center_size(center, display);
                ui.painter().image(
                    texture.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        });
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

pub fn run_viewer_with_file(path: String) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DICOM Viewer")
            .with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DICOM Viewer",
        options,
        Box::new(|_cc| Ok(Box::new(DicomViewApp::new(Some(PathBuf::from(path)))))),
    )
    .expect("failed to launch DICOM Viewer");
}
