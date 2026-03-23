use eframe::egui;

pub fn register(cc: &eframe::CreationContext) {
    let ctx = &cc.egui_ctx;

    // List of (name, bytes) pairs for generated assets. Filenames produced by scripts/resize_assets.py
    let images: &[(&str, &[u8])] = &[
        ("icon_16", include_bytes!("../assets/generated/uploade-rs_icon_16.png")),
        ("icon_32", include_bytes!("../assets/generated/uploade-rs_icon_32.png")),
        ("icon_64", include_bytes!("../assets/generated/uploade-rs_icon_64.png")),
        ("icon_128", include_bytes!("../assets/generated/uploade-rs_icon_128.png")),
        ("icon_256", include_bytes!("../assets/generated/uploade-rs_icon_256.png")),
        ("logo_128", include_bytes!("../assets/generated/uploade-rs_logo_128.png")),
        ("logo_256", include_bytes!("../assets/generated/uploade-rs_logo_256.png")),
        ("logo_512", include_bytes!("../assets/generated/uploade-rs_logo_512.png")),
    ];

    for (name, data) in images {
        if data.is_empty() {
            continue;
        }
        match image::load_from_memory(data) {
            Ok(img) => {
                let img = img.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let pixels: Vec<egui::Color32> = img
                    .chunks(4)
                    .map(|px| egui::Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]))
                    .collect();
                let mut raw: Vec<u8> = Vec::with_capacity(pixels.len() * 4);
                for c in &pixels {
                    raw.push(c.r());
                    raw.push(c.g());
                    raw.push(c.b());
                    raw.push(c.a());
                }
                let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &raw);
                let _ = ctx.load_texture((*name).to_string(), color_image, egui::TextureOptions::default());
            }
            Err(e) => {
                tracing::warn!("Failed to decode asset {}: {}", name, e);
            }
        }
    }
}
