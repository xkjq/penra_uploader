use crossbeam_channel::Sender;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::thread;

pub trait SpeechEngine: Send {
    fn name(&self) -> &str;
    /// Start the engine; send partial/final transcripts to `tx`.
    fn start(&mut self, tx: Sender<String>) -> Result<()>;
    /// Stop the engine (non-blocking)
    fn stop(&mut self);
}

/// Lightweight dummy engine used when no backend is configured.
pub struct DummyEngine {
    running: Arc<Mutex<bool>>,
}

impl DummyEngine {
    pub fn new() -> Self {
        Self { running: Arc::new(Mutex::new(false)) }
    }
}

impl SpeechEngine for DummyEngine {
    fn name(&self) -> &str { "dummy" }

    fn start(&mut self, tx: Sender<String>) -> Result<()> {
        let running = self.running.clone();
        *running.lock().unwrap() = true;
        // Spawn a thread that just sends a placeholder after a short delay
        thread::spawn(move || {
            let _ = std::thread::sleep(std::time::Duration::from_millis(500));
            if *running.lock().unwrap() {
                let _ = tx.send("(speech backend not enabled)".to_string());
            }
        });
        Ok(())
    }

    fn stop(&mut self) {
        let mut r = self.running.lock().unwrap();
        *r = false;
    }
}

// NOTE: a Vosk implementation can be provided when the feature is enabled.
// Keep the symbol available so callers can create the engine via `create_vosk_engine()`.
#[cfg(feature = "vosk")]
mod vosk_impl {
    use super::SpeechEngine;
    use crossbeam_channel::Sender;
    use anyhow::{Result, anyhow};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use cpal::traits::{HostTrait, DeviceTrait, StreamTrait};
    use cpal::{SampleFormat, StreamConfig};
    use vosk::{Model, Recognizer, DecodingState};
    use serde_json;

    pub struct VoskEngine {
        running: Arc<Mutex<bool>>,
        // keep model path for recreation if needed
        model_path: String,
        // optional handle to stop the spawned thread
    }

    impl VoskEngine {
        pub fn new(model_path: &str) -> Result<Self> {
            if model_path.is_empty() {
                return Err(anyhow!("empty model path"));
            }
            // Defer actual model loading to the start() method so creating the engine
            // doesn't fail on missing audio device at construction time.
            Ok(Self { running: Arc::new(Mutex::new(false)), model_path: model_path.to_string() })
        }
    }

    impl SpeechEngine for VoskEngine {
        fn name(&self) -> &str { "vosk" }

        fn start(&mut self, tx: Sender<String>) -> Result<()> {
            let model_path = self.model_path.clone();
            let running = self.running.clone();

            *running.lock().unwrap() = true;

            // spawn a thread to own the audio stream and recognizer
            thread::spawn(move || {
                // try to load model
                // Emit CPAL/host/device diagnostics to help debug ALSA poll errors.
                let host = cpal::default_host();
                let _ = tx.send("[dbg] cpal default host obtained".to_string());
                match host.default_input_device() {
                    Some(d) => match d.name() {
                        Ok(n) => { let _ = tx.send(format!("[dbg] default input device: {}", n)); }
                        Err(e) => { let _ = tx.send(format!("[dbg] default input device name error: {}", e)); }
                    },
                    None => { let _ = tx.send("[dbg] no default input device (cpal)".to_string()); }
                }

                match host.input_devices() {
                    Ok(devs) => {
                        for (i, dev) in devs.enumerate() {
                            let name = dev.name().unwrap_or_else(|_| "<unknown>".to_string());
                            let _ = tx.send(format!("[dbg] device {}: {}", i, name));
                        }
                    }
                    Err(e) => { let _ = tx.send(format!("[dbg] input_devices error: {:?}", e)); }
                }

                match host.default_input_device() {
                    Some(dev) => match dev.default_input_config() {
                        Ok(cfg) => {
                            let _ = tx.send(format!("[dbg] default_input_config: channels={} sample_rate={} sample_format={}", cfg.channels(), cfg.sample_rate().0, cfg.sample_format()));
                        }
                        Err(e) => { let _ = tx.send(format!("[dbg] default_input_config error: {}", e)); }
                    },
                    None => { let _ = tx.send("[dbg] no default input device (cpal)".to_string()); }
                }

                let model = match Model::new(&model_path) {
                    Some(m) => m,
                    None => {
                        let _ = tx.send(format!("vosk model load error: failed to open model at {}", model_path));
                        return;
                    }
                };

                // create host and default input device/config
                let host = cpal::default_host();
                let device = match host.default_input_device() {
                    Some(d) => d,
                    None => {
                        let _ = tx.send("no input device available".to_string());
                        return;
                    }
                };

                // Try default device first, then iterate other input devices until one works.
                let mut final_stream: Option<cpal::Stream> = None;

                // collect candidate devices (default first)
                let mut candidates: Vec<cpal::Device> = Vec::new();
                if let Some(d) = host.default_input_device() {
                    candidates.push(d);
                }
                if let Ok(devs) = host.input_devices() {
                    for d in devs {
                        // avoid duplicates by name
                        let name = d.name().unwrap_or_else(|_| "<unknown>".to_string());
                        if !candidates.iter().any(|cd| cd.name().map(|n| n==name).unwrap_or(false)) {
                            candidates.push(d);
                        }
                    }
                }

                for dev in candidates.into_iter() {
                    let dev_name = dev.name().unwrap_or_else(|_| "<unknown>".to_string());
                    let _ = tx.send(format!("[dbg] trying device: {}", dev_name));

                    let cfg = match dev.default_input_config() {
                        Ok(c) => c,
                        Err(e) => { let _ = tx.send(format!("[dbg] device {} config error: {}", dev_name, e)); continue; }
                    };

                    let sample_rate = cfg.sample_rate().0 as f32;

                    // create a recognizer for this sample rate
                    let recognizer = match Recognizer::new(&model, sample_rate) {
                        Some(r) => r,
                        None => { let _ = tx.send(format!("[dbg] recognizer create failed for {}", dev_name)); continue; }
                    };

                    let stream_config = StreamConfig {
                        channels: cfg.channels(),
                        sample_rate: cfg.sample_rate(),
                        buffer_size: cpal::BufferSize::Default,
                    };

                    let build_result = match cfg.sample_format() {
                        SampleFormat::F32 => {
                            let recognizer = Arc::new(Mutex::new(recognizer));
                            let r2 = recognizer.clone();
                            let data_tx = tx.clone();
                            let err_tx = tx.clone();
                            dev.build_input_stream(
                                &stream_config,
                                move |data: &[f32], _| {
                                    let mut rec = r2.lock().unwrap();
                                    let buf: Vec<i16> = data.iter().map(|&s| (s * i16::MAX as f32) as i16).collect();
                                    match rec.accept_waveform(&buf) {
                                        Ok(state) => match state {
                                            DecodingState::Finalized => {
                                                let finalr = rec.final_result();
                                                let s = serde_json::to_string(&finalr).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Running => {
                                                let partial = rec.partial_result();
                                                let s = serde_json::to_string(&partial).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Failed => { let _ = data_tx.send("decoding failed".to_string()); }
                                        },
                                        Err(e) => { let _ = data_tx.send(format!("accept_waveform error: {}", e)); }
                                    }
                                },
                                move |err| { let _ = err_tx.send(format!("audio input error: {:?}", err)); },
                                None,
                            )
                        }
                        SampleFormat::I16 => {
                            let recognizer = Arc::new(Mutex::new(recognizer));
                            let r2 = recognizer.clone();
                            let data_tx = tx.clone();
                            let err_tx = tx.clone();
                            dev.build_input_stream(
                                &stream_config,
                                move |data: &[i16], _| {
                                    let mut rec = r2.lock().unwrap();
                                    match rec.accept_waveform(data) {
                                        Ok(state) => match state {
                                            DecodingState::Finalized => {
                                                let finalr = rec.final_result();
                                                let s = serde_json::to_string(&finalr).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Running => {
                                                let partial = rec.partial_result();
                                                let s = serde_json::to_string(&partial).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Failed => { let _ = data_tx.send("decoding failed".to_string()); }
                                        },
                                        Err(e) => { let _ = data_tx.send(format!("accept_waveform error: {}", e)); }
                                    }
                                },
                                move |err| { let _ = err_tx.send(format!("audio input error: {:?}", err)); },
                                None,
                            )
                        }
                        SampleFormat::U16 => {
                            let recognizer = Arc::new(Mutex::new(recognizer));
                            let r2 = recognizer.clone();
                            let data_tx = tx.clone();
                            let err_tx = tx.clone();
                            dev.build_input_stream(
                                &stream_config,
                                move |data: &[u16], _| {
                                    let buf: Vec<i16> = data.iter().map(|&s| (s as i32 - 32768) as i16).collect();
                                    let mut rec = r2.lock().unwrap();
                                    match rec.accept_waveform(&buf) {
                                        Ok(state) => match state {
                                            DecodingState::Finalized => {
                                                let finalr = rec.final_result();
                                                let s = serde_json::to_string(&finalr).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Running => {
                                                let partial = rec.partial_result();
                                                let s = serde_json::to_string(&partial).unwrap_or_default();
                                                if !s.is_empty() { let _ = data_tx.send(s); }
                                            }
                                            DecodingState::Failed => { let _ = data_tx.send("decoding failed".to_string()); }
                                        },
                                        Err(e) => { let _ = data_tx.send(format!("accept_waveform error: {}", e)); }
                                    }
                                },
                                move |err| { let _ = err_tx.send(format!("audio input error: {:?}", err)); },
                                None,
                            )
                        }
                        _ => { let _ = tx.send("unsupported sample format".to_string()); continue; }
                    };

                    match build_result {
                        Ok(s) => {
                            if let Err(e) = s.play() {
                                let _ = tx.send(format!("[dbg] play failed on {}: {}", dev_name, e));
                                continue;
                            }
                            final_stream = Some(s);
                            break;
                        }
                        Err(e) => {
                            let _ = tx.send(format!("[dbg] build_input_stream failed on {}: {}", dev_name, e));
                            continue;
                        }
                    }
                }

                let stream = match final_stream {
                    Some(s) => s,
                    None => { let _ = tx.send("failed to open any input stream".to_string()); return; }
                };

                // keep the thread alive while running is true
                while *running.lock().unwrap() {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }

                // stop: drop stream and end
                drop(stream);
            });

            Ok(())
        }

        fn stop(&mut self) {
            let mut r = self.running.lock().unwrap();
            *r = false;
        }
    }

    pub fn create_vosk_engine(model_path: &str) -> Result<Box<dyn SpeechEngine>> {
        // Attempt to construct a VoskEngine which will validate the model path
        Ok(Box::new(VoskEngine::new(model_path)?))
    }
}

#[cfg(not(feature = "vosk"))]
mod vosk_impl {
    use super::{SpeechEngine, DummyEngine};
    use anyhow::Result;

    pub fn create_vosk_engine(_model_path: &str) -> Result<Box<dyn SpeechEngine>> {
        // When the Vosk feature is not enabled, return the dummy engine so the rest of
        // the app can function and the user can enable Vosk later.
        Ok(Box::new(DummyEngine::new()))
    }
}

pub use vosk_impl::create_vosk_engine;
 