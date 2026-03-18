use eframe::egui;
use egui::Vec2;
use dicom_object::{open_file, Tag};
use dicom_pixeldata::PixelDecoder;
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

/// One loaded DICOM image with its decoded pixels and metadata.
struct LoadedImage {
    raw_image: RawImage,
    metadata: Vec<(String, String)>,
}

/// Message sent from the background loading thread to the UI thread.
enum LoadMsg {
    /// One file decoded successfully. `wc_ww` is set for Gray16 images.
    Image {
        image: LoadedImage,
        wc_ww: Option<(f32, f32)>,
        filename: String,
    },
    /// A file failed to decode.
    Error(String),
}

struct LoadingState {
    rx: mpsc::Receiver<LoadMsg>,
    total: usize,
    received: usize,
    current_filename: String,
}

/// Decode a single DICOM file into a `LoadedImage`. Runs on a worker thread.
fn decode_single_file(path: &PathBuf) -> Result<(LoadedImage, Option<(f32, f32)>), String> {
    let obj = open_file(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;

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

    let rescale_intercept = get_f32(Tag(0x0028, 0x1052)).unwrap_or(0.0);
    let rescale_slope = get_f32(Tag(0x0028, 0x1053)).unwrap_or(1.0);
    let wc_dicom = get_f32(Tag(0x0028, 0x1050));
    let ww_dicom = get_f32(Tag(0x0028, 0x1051));

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

    let wc_ww = if let RawImage::Gray16(ref g) = raw_image {
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

    Ok((LoadedImage { raw_image, metadata }, wc_ww))
}

struct DicomViewApp {
    pending_load: Option<Vec<PathBuf>>,
    images: Vec<LoadedImage>,
    current_slice: usize,
    texture: Option<egui::TextureHandle>,
    zoom: f32,
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
            current_slice: 0,
            texture: None,
            zoom: 1.0,
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
        self.error = None;
        self.images.clear();
        self.texture = None;
        self.current_slice = 0;
        self.pan = Vec2::ZERO;
        self.zoom = 1.0;
        self.wl_dirty = false;

        let total = paths.len();
        let (tx, rx) = mpsc::channel::<LoadMsg>();
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            for path in paths {
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let msg = match decode_single_file(&path) {
                    Ok((image, wc_ww)) => LoadMsg::Image { image, wc_ww, filename },
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

    fn update_current_slice_view(&mut self, ctx: &egui::Context) {
        if self.images.is_empty() {
            self.texture = None;
            return;
        }
        let img = &self.images[self.current_slice];
        let (width, height) = img.raw_image.dimensions();
        let rgba = img.raw_image.to_rgba(self.window_center, self.window_width);
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
        if let Some(paths) = self.pending_load.take() {
            self.load_files(paths, ctx);
        }

        // ── Poll background loading thread ─────────────────────────────────
        let mut loading_complete = false;
        if let Some(state) = &mut self.loading {
            loop {
                match state.rx.try_recv() {
                    Ok(LoadMsg::Image { image, wc_ww, filename }) => {
                        // Set W/L from the first Gray16 image
                        if self.images.is_empty() {
                            if let Some((wc, ww)) = wc_ww {
                                self.window_center = wc;
                                self.window_width = ww;
                            }
                        }
                        state.received += 1;
                        state.current_filename = filename;
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
                if self.current_slice > 0 {
                    self.current_slice -= 1;
                    self.wl_dirty = true;
                }
            }
            if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight) {
                if self.current_slice < self.images.len().saturating_sub(1) {
                    self.current_slice += 1;
                    self.wl_dirty = true;
                }
            }
        });

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open").clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter("DICOM", &["dcm", "DCM"])
                        .add_filter("All Files", &["*"])
                        .pick_files()
                    {
                        self.pending_load = Some(paths);
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
                if !self.images.is_empty() {
                    let is_grayscale16 = self.images[self.current_slice]
                        .raw_image
                        .is_grayscale16();
                    if is_grayscale16 {
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
                }

                // Slice navigation
                if self.images.len() > 1 {
                    ui.separator();
                    ui.label(format!("Slice: {}/{}", self.current_slice + 1, self.images.len()));
                    let mut slice_val = self.current_slice as i32;
                    let slider = ui.add(
                        egui::Slider::new(&mut slice_val, 0..=(self.images.len() - 1) as i32)
                            .show_value(false)
                    );
                    if slider.changed() {
                        self.current_slice = slice_val as usize;
                        self.wl_dirty = true;
                    }
                }
            });
        });

        // ── Metadata side panel ───────────────────────────────────────────────
        if self.show_metadata && !self.images.is_empty() {
            let metadata = &self.images[self.current_slice].metadata;
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
                    ui.label("Open DICOM files or drag and drop them here");
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

                // Check button states for multi-button combinations
                let (left_down, right_down, middle_down) = ctx.input(|i| {
                    (
                        i.pointer.button_down(egui::PointerButton::Primary),
                        i.pointer.button_down(egui::PointerButton::Secondary),
                        i.pointer.button_down(egui::PointerButton::Middle),
                    )
                });

                // Scroll wheel behavior:
                // - Stack mode (multiple images): scroll navigates slices.
                // - Single-image mode: scroll zooms.
                let scroll_delta = ctx.input(|i| i.smooth_scroll_delta.y);
                if response.hovered() && scroll_delta != 0.0 {
                    if self.images.len() > 1 {
                        // Scroll to change slice
                        if scroll_delta > 0.0 && self.current_slice > 0 {
                            self.current_slice -= 1;
                            self.wl_dirty = true;
                        } else if scroll_delta < 0.0 && self.current_slice < self.images.len() - 1 {
                            self.current_slice += 1;
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
                else if response.hovered() && middle_down && self.images.len() > 1 {
                    let delta = response.drag_delta();
                    // Upward drag → previous slice, downward → next slice
                    if delta.y > 2.0 && self.current_slice > 0 {
                        self.current_slice -= 1;
                        self.wl_dirty = true;
                    } else if delta.y < -2.0 && self.current_slice < self.images.len() - 1 {
                        self.current_slice += 1;
                        self.wl_dirty = true;
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
                    if !self.images.is_empty() {
                        let is_grayscale16 = self.images[self.current_slice]
                            .raw_image
                            .is_grayscale16();
                        if is_grayscale16 {
                            let delta = response.drag_delta();
                            // Scale factor: drag 1px ≈ 2 HU change (reasonable for CT).
                            let ww_scale = 2.0_f32;
                            let wc_scale = 2.0_f32;
                            self.window_width = (self.window_width + delta.x * ww_scale).max(1.0);
                            self.window_center += -delta.y * wc_scale;
                            self.wl_dirty = true;
                        }
                    }
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
