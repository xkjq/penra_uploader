use std::path::Path;
use dicom_object::open_file;
use dicom_object::Tag;
use dicom_pixeldata::PixelDecoder;
use blake3;
use std::fs;

#[derive(Debug, Default)]
struct HashReport {
    decoded_pixel_hash: Option<String>,
    decoded_u16_byte_swapped_hash: Option<String>,
    decoded_u32_byte_swapped_hash: Option<String>,
    decoded_u16_low_bytes_hash: Option<String>,
    decoded_u16_high_bytes_hash: Option<String>,
    decoded_frame_concat_hash: Option<String>,
    raw_pixel_hash: Option<String>,
    whole_file_hash: Option<String>,
}

fn hash_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn byte_swap_words(bytes: &[u8], word_size: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for chunk in out.chunks_exact_mut(word_size) {
        chunk.reverse();
    }
    out
}

fn calc(path: &Path) -> HashReport {
    let mut out = HashReport::default();

    if let Ok(obj) = open_file(path) {
        tracing::info!("opened file OK");
        match obj.element(Tag(0x7FE0,0x0010)) {
            Ok(_) => tracing::info!("PixelData element present"),
            Err(e) => tracing::warn!("PixelData element missing or error: {}", e),
        }

        // Preferred: hash decoded pixel bytes.
        match obj.decode_pixel_data() {
            Ok(pixel_data) => {
                let bytes = pixel_data.data();
                tracing::info!("Decoded PixelData bytes len: {}", bytes.len());
                let show = &bytes[..std::cmp::min(32, bytes.len())];
                tracing::debug!("first decoded bytes: {}", hex::encode(show));
                out.decoded_pixel_hash = Some(hash_hex(bytes));

                // Additional canonicalization variants for troubleshooting
                if bytes.len() >= 2 {
                    let swapped16 = byte_swap_words(bytes, 2);
                    out.decoded_u16_byte_swapped_hash = Some(hash_hex(&swapped16));

                    let mut lows = Vec::with_capacity(bytes.len() / 2);
                    let mut highs = Vec::with_capacity(bytes.len() / 2);
                    for pair in bytes.chunks_exact(2) {
                        lows.push(pair[0]);
                        highs.push(pair[1]);
                    }
                    out.decoded_u16_low_bytes_hash = Some(hash_hex(&lows));
                    out.decoded_u16_high_bytes_hash = Some(hash_hex(&highs));
                }

                if bytes.len() >= 4 {
                    let swapped32 = byte_swap_words(bytes, 4);
                    out.decoded_u32_byte_swapped_hash = Some(hash_hex(&swapped32));
                }

                // Hash concatenated frame payloads if frame metadata can be inferred.
                let rows = obj
                    .element(Tag(0x0028, 0x0010))
                    .ok()
                    .and_then(|e| e.to_int::<u32>().ok())
                    .unwrap_or(0);
                let cols = obj
                    .element(Tag(0x0028, 0x0011))
                    .ok()
                    .and_then(|e| e.to_int::<u32>().ok())
                    .unwrap_or(0);
                let samples_per_pixel = obj
                    .element(Tag(0x0028, 0x0002))
                    .ok()
                    .and_then(|e| e.to_int::<u32>().ok())
                    .unwrap_or(1);
                let bits_allocated = obj
                    .element(Tag(0x0028, 0x0100))
                    .ok()
                    .and_then(|e| e.to_int::<u32>().ok())
                    .unwrap_or(0);
                let num_frames = obj
                    .element(Tag(0x0028, 0x0008))
                    .ok()
                    .and_then(|e| e.to_str().ok())
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(1);

                if rows > 0 && cols > 0 && bits_allocated > 0 {
                    let bytes_per_sample = (bits_allocated as usize).div_ceil(8);
                    let frame_size = rows as usize
                        * cols as usize
                        * samples_per_pixel as usize
                        * bytes_per_sample;
                    if frame_size > 0 && num_frames > 0 {
                        let expected = frame_size * num_frames;
                        tracing::info!("frame_size={} num_frames={} expected_decoded_len={}", frame_size, num_frames, expected);
                        if bytes.len() >= expected {
                            let concat = &bytes[..expected];
                            out.decoded_frame_concat_hash = Some(hash_hex(concat));
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("decode_pixel_data failed: {}", e);
            }
        }

        // Fallback: hash raw PixelData element bytes.
        if let Ok(elem) = obj.element(Tag(0x7FE0, 0x0010)) {
            if let Ok(bytes) = elem.to_bytes() {
                tracing::info!("Raw PixelData bytes len: {}", bytes.len());
                let show = &bytes[..std::cmp::min(32, bytes.len())];
                tracing::debug!("first raw bytes: {}", hex::encode(show));
                out.raw_pixel_hash = Some(blake3::hash(&bytes).to_hex().to_string());
            }
            if let Ok(s) = elem.to_str() {
                tracing::info!("PixelData as str len: {}", s.as_bytes().len());
                out.raw_pixel_hash = Some(blake3::hash(s.as_bytes()).to_hex().to_string());
            }
        }
    }

    // Always compute whole-file hash for side-by-side comparison.
    if let Ok(b) = fs::read(path) {
        tracing::info!("Hashing whole file bytes len: {}", b.len());
        out.whole_file_hash = Some(blake3::hash(&b).to_hex().to_string());
    }

    out
}

fn main() {
    // initialize basic tracing for this utility so internal info/debug
    // messages are emitted to stderr.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .with_target(false)
        .try_init();
    let arg = std::env::args().nth(1).expect("provide path");
    let p = Path::new(&arg);
    let report = calc(p);
    println!("DECODED_PIXEL_HASH:{}", report.decoded_pixel_hash.as_deref().unwrap_or("<none>"));
    println!(
        "DECODED_U16_BYTE_SWAPPED_HASH:{}",
        report
            .decoded_u16_byte_swapped_hash
            .as_deref()
            .unwrap_or("<none>")
    );
    println!(
        "DECODED_U32_BYTE_SWAPPED_HASH:{}",
        report
            .decoded_u32_byte_swapped_hash
            .as_deref()
            .unwrap_or("<none>")
    );
    println!(
        "DECODED_U16_LOW_BYTES_HASH:{}",
        report
            .decoded_u16_low_bytes_hash
            .as_deref()
            .unwrap_or("<none>")
    );
    println!(
        "DECODED_U16_HIGH_BYTES_HASH:{}",
        report
            .decoded_u16_high_bytes_hash
            .as_deref()
            .unwrap_or("<none>")
    );
    println!(
        "DECODED_FRAME_CONCAT_HASH:{}",
        report
            .decoded_frame_concat_hash
            .as_deref()
            .unwrap_or("<none>")
    );
    println!("RAW_PIXEL_HASH:{}", report.raw_pixel_hash.as_deref().unwrap_or("<none>"));
    println!("WHOLE_FILE_HASH:{}", report.whole_file_hash.as_deref().unwrap_or("<none>"));
}
