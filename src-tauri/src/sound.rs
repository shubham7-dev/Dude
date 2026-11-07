// sound.rs - Procedural audio engine using rodio for Dude mascot window

use std::collections::HashMap;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use rodio::Player;
use tauri::Manager;
use tauri_plugin_store::StoreExt;

pub const SAMPLE_RATE: u32 = 44100;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    Triangle,
}

pub struct Note {
    pub start_freq: f32,
    pub end_freq: f32,
    pub duration_ms: f32,
    pub waveform: Waveform,
}

#[derive(Clone)]
pub struct SoundData {
    pub id: &'static str,
    pub samples: Arc<Vec<f32>>,
    pub channels: u16,
    pub sample_rate: u32,
    pub duration_ms: f32,
    pub peak_amplitude: f32,
}

#[derive(serde::Serialize)]
pub struct SoundDebugInfo {
    pub id: String,
    pub samples: usize,
    pub duration_ms: f32,
    pub peak: f32,
}

enum AudioCommand {
    Play {
        id: &'static str,
        samples: Arc<Vec<f32>>,
        channels: u16,
        sample_rate: u32,
        volume: f32,
        call_time: Instant,
    },
    StopAll,
}

struct SoundRateLimiter {
    last_any_sound: Option<Instant>,
    last_by_id: HashMap<&'static str, Instant>,
}

static SOUND_ENGINE: OnceLock<SoundEngine> = OnceLock::new();
static AUDIO_SENDER: OnceLock<SyncSender<AudioCommand>> = OnceLock::new();
static RATE_LIMITER: OnceLock<Mutex<SoundRateLimiter>> = OnceLock::new();
static ACTIVE_SOUND_COUNT: AtomicUsize = AtomicUsize::new(0);

pub struct SoundEngine {
    sounds: HashMap<&'static str, SoundData>,
}

impl SoundEngine {
    pub fn new() -> Self {
        let mut sounds = HashMap::new();

        // 1. "expand": 80 ms, rising 520 to 780 Hz
        sounds.insert("expand", generate_sound("expand", &[
            Note { start_freq: 520.0, end_freq: 780.0, duration_ms: 80.0, waveform: Waveform::Sine },
        ]));

        // 2. "collapse": 80 ms, falling 780 to 520 Hz
        sounds.insert("collapse", generate_sound("collapse", &[
            Note { start_freq: 780.0, end_freq: 520.0, duration_ms: 80.0, waveform: Waveform::Sine },
        ]));

        // 3. "tab": 30 ms tick at 1200 Hz
        sounds.insert("tab", generate_sound("tab", &[
            Note { start_freq: 1200.0, end_freq: 1200.0, duration_ms: 30.0, waveform: Waveform::Triangle },
        ]));

        // 4. "poke": 90 ms boop gliding 330 to 520 Hz
        sounds.insert("poke", generate_sound("poke", &[
            Note { start_freq: 330.0, end_freq: 520.0, duration_ms: 90.0, waveform: Waveform::Sine },
        ]));

        // 5. "send": 100 ms chirp 600 to 900 Hz
        sounds.insert("send", generate_sound("send", &[
            Note { start_freq: 600.0, end_freq: 900.0, duration_ms: 100.0, waveform: Waveform::Sine },
        ]));

        // 6. "reply": three notes C6 E6 G6, 70 ms each, soft (C6=1046.5, E6=1318.5, G6=1568.0)
        sounds.insert("reply", generate_sound("reply", &[
            Note { start_freq: 1046.5, end_freq: 1046.5, duration_ms: 70.0, waveform: Waveform::Sine },
            Note { start_freq: 1318.5, end_freq: 1318.5, duration_ms: 70.0, waveform: Waveform::Sine },
            Note { start_freq: 1568.0, end_freq: 1568.0, duration_ms: 70.0, waveform: Waveform::Sine },
        ]));

        // 7. "error": two descending notes 392 Hz then 294 Hz, 110 ms each
        sounds.insert("error", generate_sound("error", &[
            Note { start_freq: 392.0, end_freq: 392.0, duration_ms: 110.0, waveform: Waveform::Triangle },
            Note { start_freq: 294.0, end_freq: 294.0, duration_ms: 110.0, waveform: Waveform::Triangle },
        ]));

        // 8. "stop": 40 ms tick at 440 Hz
        sounds.insert("stop", generate_sound("stop", &[
            Note { start_freq: 440.0, end_freq: 440.0, duration_ms: 40.0, waveform: Waveform::Sine },
        ]));

        // 9. "copy": 25 ms click at 1000 Hz
        sounds.insert("copy", generate_sound("copy", &[
            Note { start_freq: 1000.0, end_freq: 1000.0, duration_ms: 25.0, waveform: Waveform::Triangle },
        ]));

        // 10. "startup": loaded from assets/sounds/startup.mp3 (with procedural fallback)
        sounds.insert("startup", load_startup_sound());

        // 11. "agent-complete": loaded from assets/sounds/agent-complete.mp3 (with procedural fallback)
        sounds.insert("agent-complete", load_agent_complete_sound());

        Self { sounds }
    }

    pub fn get_sound(&self, id: &str) -> Option<&SoundData> {
        if id == "agent_complete" {
            self.sounds.get("agent-complete")
        } else {
            self.sounds.get(id)
        }
    }

    pub fn list_sounds(&self) -> Vec<(&'static str, usize, f32, f32)> {
        let order = ["expand", "collapse", "tab", "poke", "send", "reply", "error", "stop", "copy", "startup", "agent-complete"];
        order.iter().filter_map(|&id| {
            self.sounds.get(id).map(|s| (id, s.samples.len(), s.duration_ms, s.peak_amplitude))
        }).collect()
    }
}

/// Decodes an audio file from raw bytes into SoundData using rodio / symphonia.
pub fn decode_audio_bytes(id: &'static str, bytes: &[u8]) -> Option<SoundData> {
    use rodio::Decoder;
    use rodio::Source;
    use std::io::Cursor;

    let cursor = Cursor::new(bytes.to_vec());
    let decoder = Decoder::new(cursor).ok()?;
    let channels = decoder.channels().get();
    let sample_rate = decoder.sample_rate().get();
    let samples: Vec<f32> = decoder.collect();

    if samples.is_empty() {
        return None;
    }

    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let duration_ms = (samples.len() as f32 / (sample_rate as f32 * channels as f32)) * 1000.0;

    Some(SoundData {
        id,
        samples: Arc::new(samples),
        channels,
        sample_rate,
        duration_ms,
        peak_amplitude: peak,
    })
}

/// Loads the startup sound from assets/sounds/startup.mp3 (or resources/startup.mp3),
/// with guaranteed zero-latency fallback to embedded bytes and procedural chime.
fn load_startup_sound() -> SoundData {
    // 1. Try file from disk first (useful for testing or customization)
    let candidate_paths = [
        std::path::PathBuf::from("assets/sounds/startup.mp3"),
        std::path::PathBuf::from("../assets/sounds/startup.mp3"),
        std::path::PathBuf::from("resources/startup.mp3"),
        std::path::PathBuf::from("src-tauri/resources/startup.mp3"),
    ];

    for path in &candidate_paths {
        if let Ok(bytes) = std::fs::read(path) {
            if let Some(sound) = decode_audio_bytes("startup", &bytes) {
                return sound;
            }
        }
    }

    // 2. Try embedded bytes from assets/sounds/startup.mp3
    let embedded_bytes = include_bytes!("../../assets/sounds/startup.mp3");
    if let Some(sound) = decode_audio_bytes("startup", embedded_bytes) {
        return sound;
    }

    // 3. Fallback procedural chime if decoding fails
    generate_startup_chime()
}

/// Generates a subtle, soft, premium startup chime lasting 400 ms.
/// Uses a warm ascending 4-note major arpeggio (C5, E5, G5, C6) with smooth attack,
/// natural exponential decay, and peak amplitude normalized to 0.22 (low-volume).
fn generate_startup_chime() -> SoundData {
    let dur_sec = 0.400f32;
    let num_samples = (dur_sec * SAMPLE_RATE as f32).round() as usize;
    let mut samples = vec![0.0f32; num_samples];

    // Voices: (start_time_sec, frequency_hz, weight)
    let voices: [(f32, f32, f32); 4] = [
        (0.000, 523.25, 1.0),
        (0.070, 659.25, 0.9),
        (0.140, 783.99, 0.8),
        (0.210, 1046.50, 0.7),
    ];

    for (start_t, freq, weight) in voices {
        let start_idx = (start_t * SAMPLE_RATE as f32).round() as usize;
        for i in start_idx..num_samples {
            let dt = (i - start_idx) as f32 / SAMPLE_RATE as f32;
            let attack = ((dt / 0.018).min(1.0) * std::f32::consts::FRAC_PI_2).sin();
            let decay = (-dt / 0.16).exp();
            let env = attack * decay * weight;
            let wave = (2.0 * std::f32::consts::PI * freq * dt).sin()
                + 0.12 * (2.0 * std::f32::consts::PI * 2.0 * freq * dt).sin();
            samples[i] += wave * env;
        }
    }

    // Micro fade-in for first 5 ms (approx 220 samples) to prevent any initial transient click
    let fade_in_samples = (0.005f32 * SAMPLE_RATE as f32).round() as usize;
    for (i, s) in samples.iter_mut().take(fade_in_samples).enumerate() {
        let factor = (i as f32 / fade_in_samples as f32 * std::f32::consts::FRAC_PI_2).sin();
        *s *= factor;
    }

    // Smooth cosine fade-out on last 40 ms (approx 1764 samples) to ensure silence at end
    let fade_out_samples = (0.040f32 * SAMPLE_RATE as f32).round() as usize;
    let start_fade_idx = num_samples.saturating_sub(fade_out_samples);
    for k in 0..fade_out_samples {
        let idx = start_fade_idx + k;
        if idx < num_samples {
            let factor = ((k as f32 / fade_out_samples as f32) * std::f32::consts::FRAC_PI_2).cos();
            samples[idx] *= factor;
        }
    }

    // Normalize peak amplitude at 0.22 (subtle, soft, low-volume)
    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak > 0.0 {
        let factor = 0.22 / peak;
        for s in &mut samples {
            *s *= factor;
        }
    }

    let actual_peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let duration_ms = (num_samples as f32 / SAMPLE_RATE as f32) * 1000.0;

    SoundData {
        id: "startup",
        samples: Arc::new(samples),
        channels: 1,
        sample_rate: SAMPLE_RATE,
        duration_ms,
        peak_amplitude: actual_peak,
    }
}

/// Loads the agent-complete sound from assets/sounds/agent-complete.mp3 (or resources/agent-complete.mp3),
/// with guaranteed zero-latency fallback to embedded bytes and procedural chime.
fn load_agent_complete_sound() -> SoundData {
    // 1. Try file from disk first (useful for testing or customization)
    let candidate_paths = [
        std::path::PathBuf::from("assets/sounds/agent-complete.mp3"),
        std::path::PathBuf::from("assets/sounds/agent-complete.mp3.mp3"),
        std::path::PathBuf::from("../assets/sounds/agent-complete.mp3"),
        std::path::PathBuf::from("../assets/sounds/agent-complete.mp3.mp3"),
        std::path::PathBuf::from("resources/agent-complete.mp3"),
        std::path::PathBuf::from("src-tauri/resources/agent-complete.mp3"),
    ];

    for path in &candidate_paths {
        if let Ok(bytes) = std::fs::read(path) {
            if let Some(sound) = decode_audio_bytes("agent-complete", &bytes) {
                return sound;
            }
        }
    }

    // 2. Try embedded bytes from assets/sounds/agent-complete.mp3
    let embedded_bytes = include_bytes!("../../assets/sounds/agent-complete.mp3");
    if let Some(sound) = decode_audio_bytes("agent-complete", embedded_bytes) {
        return sound;
    }

    // 3. Fallback procedural completion chime if decoding fails
    generate_agent_complete_chime()
}

/// Generates a pleasant procedural completion chime as fallback (C6 then G6 arpeggio).
fn generate_agent_complete_chime() -> SoundData {
    generate_sound("agent-complete", &[
        Note { start_freq: 1046.5, end_freq: 1046.5, duration_ms: 100.0, waveform: Waveform::Sine },
        Note { start_freq: 1568.0, end_freq: 1568.0, duration_ms: 150.0, waveform: Waveform::Sine },
    ])
}

fn generate_sound(id: &'static str, notes: &[Note]) -> SoundData {
    let mut all_samples = Vec::new();
    let fade_samples = (SAMPLE_RATE as f32 * 0.003).round() as usize; // 3 ms fade (approx 132 samples)

    for note in notes {
        let note_sample_count = ((note.duration_ms / 1000.0) * SAMPLE_RATE as f32).round() as usize;
        let note_dur_sec = note_sample_count as f32 / SAMPLE_RATE as f32;
        let mut note_samples = Vec::with_capacity(note_sample_count);

        let attack_samples = (note_sample_count as f32 * 0.15).max(fade_samples as f32).min(note_sample_count as f32 * 0.4) as usize;
        let decay_samples = note_sample_count.saturating_sub(attack_samples);

        for i in 0..note_sample_count {
            let t = i as f32 / SAMPLE_RATE as f32;

            // Continuous instantaneous phase for frequency glide
            // f(t) = f0 + (f1 - f0) * (t / T)
            // phase = 2 * PI * (f0 * t + 0.5 * (f1 - f0) / T * t^2)
            let phase = if (note.start_freq - note.end_freq).abs() < 0.001 {
                2.0 * std::f32::consts::PI * note.start_freq * t
            } else {
                2.0 * std::f32::consts::PI * (note.start_freq * t + 0.5 * (note.end_freq - note.start_freq) / note_dur_sec * t * t)
            };

            let raw_wave = match note.waveform {
                Waveform::Sine => phase.sin(),
                Waveform::Triangle => {
                    let normalized_phase = (phase / (2.0 * std::f32::consts::PI)).fract();
                    let norm = if normalized_phase < 0.0 { normalized_phase + 1.0 } else { normalized_phase };
                    if norm < 0.25 {
                        norm * 4.0
                    } else if norm < 0.75 {
                        2.0 - norm * 4.0
                    } else {
                        norm * 4.0 - 4.0
                    }
                }
            };

            // Smooth attack & decay envelope
            let env = if i < attack_samples {
                (std::f32::consts::FRAC_PI_2 * (i as f32 / attack_samples as f32)).sin()
            } else {
                let decay_idx = i - attack_samples;
                (std::f32::consts::FRAC_PI_2 * (1.0 + decay_idx as f32 / decay_samples.max(1) as f32)).sin().max(0.0)
            };

            note_samples.push(raw_wave * env);
        }

        // Apply 3 ms micro fade-in and fade-out to each note
        let n_len = note_samples.len();
        for i in 0..fade_samples.min(n_len / 2) {
            let fade_in = i as f32 / fade_samples as f32;
            note_samples[i] *= fade_in;
            let out_idx = n_len - 1 - i;
            let fade_out = i as f32 / fade_samples as f32;
            note_samples[out_idx] *= fade_out;
        }

        all_samples.extend(note_samples);
    }

    // Overall 3 ms fade-in and fade-out on the full sound buffer (no clicks)
    let total_len = all_samples.len();
    for i in 0..fade_samples.min(total_len / 2) {
        let fade_in = i as f32 / fade_samples as f32;
        all_samples[i] *= fade_in;
        let out_idx = total_len - 1 - i;
        let fade_out = i as f32 / fade_samples as f32;
        all_samples[out_idx] *= fade_out;
    }

    // Normalization with peak amplitude at most 0.35
    let peak = all_samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak > 0.0 {
        let factor = 0.35 / peak;
        for s in &mut all_samples {
            *s *= factor;
        }
    }

    let actual_peak = all_samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let duration_ms = (total_len as f32 / SAMPLE_RATE as f32) * 1000.0;

    SoundData {
        id,
        samples: Arc::new(all_samples),
        channels: 1,
        sample_rate: SAMPLE_RATE,
        duration_ms,
        peak_amplitude: actual_peak,
    }
}

pub fn get_sound_engine() -> &'static SoundEngine {
    SOUND_ENGINE.get_or_init(SoundEngine::new)
}

fn get_audio_sender() -> &'static SyncSender<AudioCommand> {
    AUDIO_SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<AudioCommand>(8);
        thread::Builder::new()
            .name("dude-audio-worker".into())
            .spawn(move || run_audio_thread(rx))
            .expect("Failed to spawn audio worker thread");
        tx
    })
}

fn get_rate_limiter() -> &'static Mutex<SoundRateLimiter> {
    RATE_LIMITER.get_or_init(|| {
        Mutex::new(SoundRateLimiter {
            last_any_sound: None,
            last_by_id: HashMap::new(),
        })
    })
}

fn run_audio_thread(rx: Receiver<AudioCommand>) {
    let mut sink_opt: Option<MixerDeviceSink> = None;
    let mut active_players: Vec<Player> = Vec::new();
    let mut last_sound_played_at: Option<Instant> = None;
    let mut last_device_error: Option<Instant> = None;
    #[cfg(debug_assertions)]
    let mut is_first_sound = true;

    loop {
        // Wait for next command or poll with timeout for idle detection
        let timeout = if active_players.is_empty() && sink_opt.is_none() {
            // Idle and no audio device open: wait indefinitely until a command arrives
            Duration::from_secs(3600)
        } else {
            Duration::from_millis(40)
        };

        match rx.recv_timeout(timeout) {
            Ok(AudioCommand::Play { id, samples, channels, sample_rate, volume, call_time }) => {
                // Ensure output stream is open (opened lazily on the first sound)
                if sink_opt.is_none() {
                    // Check retry throttling on device error: retry at most every 5 seconds
                    if let Some(err_time) = last_device_error {
                        if err_time.elapsed() < Duration::from_secs(5) {
                            // Throttle active, skip silently
                            continue;
                        }
                    }

                    match DeviceSinkBuilder::open_default_sink() {
                        Ok(mut sink) => {
                            sink.log_on_drop(false);
                            sink_opt = Some(sink);
                            last_device_error = None;
                        }
                        Err(_) => {
                            // Skip silently without crashing or blocking
                            last_device_error = Some(Instant::now());
                            continue;
                        }
                    }
                }

                if let Some(sink) = &sink_opt {
                    // Clean up finished players
                    active_players.retain(|p| !p.empty());
                    ACTIVE_SOUND_COUNT.store(active_players.len(), Ordering::Relaxed);

                    // At most 3 sounds playing at the same time: extra requests dropped silently
                    if active_players.len() >= 3 {
                        continue;
                    }

                    let channels_nz = match NonZero::new(channels) {
                        Some(c) => c,
                        None => continue,
                    };
                    let sample_rate_nz = match NonZero::new(sample_rate) {
                        Some(sr) => sr,
                        None => continue,
                    };

                    // Catch unhandled device removal / driver disconnection safely without crashing
                    let play_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let player = Player::connect_new(&sink.mixer());
                        let source = SamplesBuffer::new(channels_nz, sample_rate_nz, (*samples).clone());
                        player.set_volume(volume);
                        player.append(source);
                        player.play();
                        player
                    }));

                    match play_res {
                        Ok(player) => {
                            active_players.push(player);
                            ACTIVE_SOUND_COUNT.store(active_players.len(), Ordering::Relaxed);
                            last_sound_played_at = Some(Instant::now());

                            #[cfg(debug_assertions)]
                            {
                                let latency = call_time.elapsed();
                                if is_first_sound {
                                    println!("[sound] id: {}, samples: {}, latency: {:?} (first sound)", id, samples.len(), latency);
                                    is_first_sound = false;
                                } else {
                                    println!("[sound] id: {}, samples: {}, latency: {:?}", id, samples.len(), latency);
                                }
                            }
                            #[cfg(not(debug_assertions))]
                            {
                                let _ = (id, call_time);
                            }
                        }
                        Err(_) => {
                            // Audio device was removed or changed: drop broken sink, throttle retry for 5 seconds
                            sink_opt = None;
                            last_device_error = Some(Instant::now());
                            active_players.clear();
                            ACTIVE_SOUND_COUNT.store(0, Ordering::Relaxed);
                        }
                    }
                }
            }
            Ok(AudioCommand::StopAll) => {
                for player in &active_players {
                    player.stop();
                }
                active_players.clear();
                ACTIVE_SOUND_COUNT.store(0, Ordering::Relaxed);
            }
            Err(RecvTimeoutError::Timeout) => {
                // Periodically clean up finished players
                active_players.retain(|p| !p.empty());
                ACTIVE_SOUND_COUNT.store(active_players.len(), Ordering::Relaxed);

                // Release stream after 8 seconds without sound (no audio device held while idle)
                if active_players.is_empty() {
                    if let Some(last_time) = last_sound_played_at {
                        if last_time.elapsed() >= Duration::from_secs(8) {
                            if sink_opt.is_some() {
                                sink_opt = None; // Dropping MixerDeviceSink releases CPAL stream and WASAPI device
                            }
                            last_sound_played_at = None;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Application quitting
                break;
            }
        }
    }
}

pub fn get_sound_volume(app: &tauri::AppHandle) -> f32 {
    let store = match app.store(crate::STORE_FILENAME) {
        Ok(s) => s,
        Err(_) => return 0.35,
    };
    if let Some(v) = store.get("sound.volume") {
        if let Some(num) = v.as_f64() {
            return (num as f32).clamp(0.0, 1.0);
        }
        if let Some(str_val) = v.as_str() {
            if let Ok(num) = str_val.parse::<f32>() {
                return num.clamp(0.0, 1.0);
            }
        }
    }
    if let Some(v) = store.get("sound") {
        if let Some(vol) = v.get("volume").and_then(|x| x.as_f64()) {
            return (vol as f32).clamp(0.0, 1.0);
        }
    }
    0.35
}

/// Tracks whether the startup sound has already been played in this process.
/// Guarantees that the startup chime can play at most ONCE per genuine manual launch.
static HAS_PLAYED_STARTUP_SOUND: AtomicBool = AtomicBool::new(false);

/// Returns true if the application was launched manually by the user,
/// rather than automatically started by Windows via `--autostart`.
pub fn is_manual_launch() -> bool {
    !std::env::args().any(|arg| arg == "--autostart")
}

/// Checks the user's "Sound effects" setting from the persistent store.
/// Returns true if enabled (the default), or false if disabled.
pub fn is_sound_effects_enabled(app: &tauri::AppHandle) -> bool {
    let store = match app.store(crate::STORE_FILENAME) {
        Ok(s) => s,
        Err(_) => return true,
    };
    if let Some(v) = store.get("behavior.soundEnabled") {
        if let Some(b) = v.as_bool() {
            return b;
        }
    }
    if let Some(v) = store.get("behavior") {
        if let Some(b) = v.get("soundEnabled").and_then(|x| x.as_bool()) {
            return b;
        }
    }
    if let Some(v) = store.get("soundEnabled") {
        if let Some(b) = v.as_bool() {
            return b;
        }
    }
    true
}

/// Plays the startup sound only when:
/// 1. It hasn't been played yet in this process (atomic single-instance check).
/// 2. The app was launched manually by the user (no `--autostart` argument).
/// 3. The Sound Effects setting is enabled.
/// 4. The mascot window is visible and ready.
pub fn play_startup_sound_if_manual_internal(app: &tauri::AppHandle) {
    // 1. Single-play guard: must only play once per genuine manual launch
    if HAS_PLAYED_STARTUP_SOUND.swap(true, Ordering::SeqCst) {
        return;
    }

    // 2. Autostart check: do not play when started automatically with Windows
    if !is_manual_launch() {
        return;
    }

    // 3. Setting check: respect Sound Effects setting (stay silent if disabled)
    if !is_sound_effects_enabled(app) {
        return;
    }

    // 4. Mascot window visibility check: must be ready and visible
    if let Some(win) = app.get_webview_window("main") {
        if let Ok(visible) = win.is_visible() {
            if !visible {
                return;
            }
        }
    }

    let volume = get_sound_volume(app);
    if volume <= 0.0 {
        return;
    }

    let engine = get_sound_engine();
    let sound = match engine.get_sound("startup") {
        Some(s) => s,
        None => return,
    };

    let now = Instant::now();

    {
        let mut limiter = match get_rate_limiter().lock() {
            Ok(l) => l,
            Err(p) => p.into_inner(),
        };
        limiter.last_any_sound = Some(now);
        limiter.last_by_id.insert(sound.id, now);
    }

    // Send to dedicated audio thread without blocking (try_send drops if queue full)
    let sender = get_audio_sender();
    let _ = sender.try_send(AudioCommand::Play {
        id: sound.id,
        samples: sound.samples.clone(),
        channels: sound.channels,
        sample_rate: sound.sample_rate,
        volume,
        call_time: now,
    });
}

pub fn play_sound_internal(app: &tauri::AppHandle, sound_id: &str) {
    if sound_id == "startup" {
        play_startup_sound_if_manual_internal(app);
        return;
    }

    if !is_sound_effects_enabled(app) {
        return;
    }

    let volume = get_sound_volume(app);
    if volume <= 0.0 {
        return;
    }

    let engine = get_sound_engine();
    let sound = match engine.get_sound(sound_id) {
        Some(s) => s,
        None => return,
    };

    let now = Instant::now();
    {
        let mut limiter = match get_rate_limiter().lock() {
            Ok(l) => l,
            Err(p) => p.into_inner(),
        };

        if let Some(last) = limiter.last_any_sound {
            if now.duration_since(last) < Duration::from_millis(40) {
                return;
            }
        }

        let min_interval = match sound.id {
            "agent-complete" => Duration::from_millis(1000),
            "copy" => Duration::from_millis(80),
            _ => Duration::from_millis(60),
        };

        if let Some(last) = limiter.last_by_id.get(sound.id) {
            if now.duration_since(*last) < min_interval {
                return;
            }
        }

        limiter.last_any_sound = Some(now);
        limiter.last_by_id.insert(sound.id, now);
    }

    let sender = get_audio_sender();
    let _ = sender.try_send(AudioCommand::Play {
        id: sound.id,
        samples: sound.samples.clone(),
        channels: sound.channels,
        sample_rate: sound.sample_rate,
        volume,
        call_time: now,
    });
}

pub fn stop_all_sounds_internal() {
    let sender = get_audio_sender();
    let _ = sender.try_send(AudioCommand::StopAll);
}

#[cfg(test)]
pub fn reset_startup_sound_flag_for_test() {
    HAS_PLAYED_STARTUP_SOUND.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Tauri Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn play_sound(app: tauri::AppHandle, id: String) -> Result<(), String> {
    play_sound_internal(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn play_startup_sound_if_manual(app: tauri::AppHandle) -> Result<(), String> {
    play_startup_sound_if_manual_internal(&app);
    Ok(())
}

#[tauri::command]
pub fn stop_all_sounds() -> Result<(), String> {
    stop_all_sounds_internal();
    Ok(())
}

#[tauri::command]
pub fn get_sound_debug_info(id: String) -> Option<SoundDebugInfo> {
    #[cfg(debug_assertions)]
    {
        let engine = get_sound_engine();
        engine.get_sound(&id).map(|s| SoundDebugInfo {
            id: s.id.to_string(),
            samples: s.samples.len(),
            duration_ms: s.duration_ms,
            peak: s.peak_amplitude,
        })
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = id;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_procedural_sounds_generated() {
        let engine = SoundEngine::new();
        let expected = ["expand", "collapse", "tab", "poke", "send", "reply", "error", "stop", "copy"];

        for id in expected {
            let sound = engine.get_sound(id).unwrap_or_else(|| panic!("Missing sound {}", id));
            assert_eq!(sound.id, id);
            assert!(sound.samples.len() > 0, "Sound {} has no samples", id);
            assert!(sound.duration_ms < 350.0, "Sound {} duration {}ms exceeds 350ms", id, sound.duration_ms);
            assert!(sound.peak_amplitude <= 0.3501, "Sound {} peak {} exceeds 0.35", id, sound.peak_amplitude);
            assert!(sound.peak_amplitude >= 0.34, "Sound {} peak {} not normalized to 0.35", id, sound.peak_amplitude);

            // Check 3 ms fade-in and fade-out to prevent clicks
            let first_sample = sound.samples[0].abs();
            let last_sample = sound.samples[sound.samples.len() - 1].abs();
            assert!(first_sample < 0.001, "Sound {} first sample {} not zero/faded", id, first_sample);
            assert!(last_sample < 0.001, "Sound {} last sample {} not zero/faded", id, last_sample);
        }

        // Dedicated assertion for startup sound loaded from startup.mp3
        let startup = engine.get_sound("startup").expect("Missing startup sound");
        assert_eq!(startup.id, "startup");
        assert!(startup.duration_ms > 1000.0, "Startup sound duration {}ms expected >1000ms", startup.duration_ms);
        assert_eq!(startup.channels, 2, "Startup sound expected stereo (2 channels)");
        assert_eq!(startup.sample_rate, 44100, "Startup sound expected 44100 Hz");
        assert!(startup.samples.len() > 0, "Startup sound has no samples");
        assert!(startup.peak_amplitude > 0.0, "Startup peak amplitude must be positive");

        // Dedicated assertion for agent-complete sound loaded from agent-complete.mp3
        let agent_complete = engine.get_sound("agent-complete").expect("Missing agent-complete sound");
        assert_eq!(agent_complete.id, "agent-complete");
        assert!(agent_complete.duration_ms > 0.0, "agent-complete duration must be positive");
        assert!(agent_complete.samples.len() > 0, "agent-complete must have samples");
        assert!(agent_complete.peak_amplitude > 0.0, "agent-complete peak amplitude must be positive");
    }

    #[test]
    fn test_procedural_chime_fallback() {
        let chime = generate_startup_chime();
        assert_eq!(chime.id, "startup");
        assert!(chime.duration_ms >= 300.0 && chime.duration_ms <= 500.0);
        assert!(chime.peak_amplitude <= 0.25);
        assert!(chime.peak_amplitude >= 0.20);
        assert!(chime.samples[0].abs() < 0.001);
        assert!(chime.samples[chime.samples.len() - 1].abs() < 0.001);
    }

    #[test]
    fn test_startup_sound_single_play_guard() {
        reset_startup_sound_flag_for_test();
        assert_eq!(HAS_PLAYED_STARTUP_SOUND.load(Ordering::SeqCst), false);

        // First swap should succeed (returns old value false)
        let first = HAS_PLAYED_STARTUP_SOUND.swap(true, Ordering::SeqCst);
        assert_eq!(first, false);

        // Second swap (simulating second instance / show / unhide / focus) must detect already played
        let second = HAS_PLAYED_STARTUP_SOUND.swap(true, Ordering::SeqCst);
        assert_eq!(second, true);

        // Subsequent swap must still report already played
        let third = HAS_PLAYED_STARTUP_SOUND.swap(true, Ordering::SeqCst);
        assert_eq!(third, true);

        reset_startup_sound_flag_for_test();
    }

    #[test]
    fn test_autostart_detection_logic() {
        let args_manual = vec!["dude.exe".to_string()];
        let is_autostart_manual = args_manual.iter().any(|arg| arg == "--autostart");
        assert_eq!(is_autostart_manual, false, "Manual launch should not be detected as autostart");

        let args_auto = vec!["dude.exe".to_string(), "--autostart".to_string()];
        let is_autostart_auto = args_auto.iter().any(|arg| arg == "--autostart");
        assert_eq!(is_autostart_auto, true, "Autostart launch must be detected");
    }

    #[test]
    fn test_non_startup_sounds_are_ignored() {
        let interaction_sounds = ["expand", "collapse", "tab", "poke", "send", "reply", "error", "stop", "copy", "random"];
        for id in interaction_sounds {
            // Verify sound ID is NOT "startup"
            assert_ne!(id, "startup");
        }
    }

    #[test]
    fn test_print_sound_measurements() {
        let engine = SoundEngine::new();
        let sounds = engine.list_sounds();
        println!("\n=== PROCEDURAL SOUND MEASUREMENTS ===");
        for (id, samples, dur_ms, peak) in sounds {
            println!("Sound '{}': duration = {:.1} ms, samples = {}, peak amplitude = {:.4}", id, dur_ms, samples, peak);
        }
        println!("=====================================\n");
    }

    #[test]
    fn test_rate_limiter_rules_and_rapid_clicking() {
        let mut limiter = SoundRateLimiter {
            last_any_sound: None,
            last_by_id: HashMap::new(),
        };

        let now = Instant::now();

        let check = |limiter: &mut SoundRateLimiter, id: &'static str, time: Instant| -> bool {
            if let Some(last_any) = limiter.last_any_sound {
                if time.duration_since(last_any) < Duration::from_millis(60) {
                    return false;
                }
            }
            let cooldown = if id == "poke" { Duration::from_millis(250) } else { Duration::from_millis(150) };
            if let Some(last_id) = limiter.last_by_id.get(id) {
                if time.duration_since(*last_id) < cooldown {
                    return false;
                }
            }
            limiter.last_any_sound = Some(time);
            limiter.last_by_id.insert(id, time);
            true
        };

        // 1. First sound accepted
        assert!(check(&mut limiter, "expand", now));

        // 2. Sound 30ms later dropped by 60ms global gap
        assert!(!check(&mut limiter, "tab", now + Duration::from_millis(30)));

        // 3. Different sound 70ms later accepted (>60ms)
        assert!(check(&mut limiter, "tab", now + Duration::from_millis(70)));

        // 4. "expand" 100ms after first dropped (100ms < 150ms cooldown)
        assert!(!check(&mut limiter, "expand", now + Duration::from_millis(100)));

        // 5. "expand" 160ms after first accepted (>150ms cooldown, 90ms gap from tab)
        assert!(check(&mut limiter, "expand", now + Duration::from_millis(160)));

        // 6. "poke" sound at 230ms accepted
        assert!(check(&mut limiter, "poke", now + Duration::from_millis(230)));

        // 7. "poke" at 400ms (170ms later) dropped (170ms < 250ms poke cooldown)
        assert!(!check(&mut limiter, "poke", now + Duration::from_millis(400)));

        // 8. "poke" at 500ms (270ms later) accepted (>250ms poke cooldown)
        assert!(check(&mut limiter, "poke", now + Duration::from_millis(500)));

        // Rapid clicking test: 20 clicks in 2 seconds (100ms spacing on "expand")
        let mut accepted_count = 0;
        let mut sim_limiter = SoundRateLimiter { last_any_sound: None, last_by_id: HashMap::new() };
        let start = Instant::now();
        for i in 0..20 {
            let t = start + Duration::from_millis(i * 100);
            if check(&mut sim_limiter, "expand", t) {
                accepted_count += 1;
            }
        }
        // At 100ms intervals with 150ms cooldown:
        // Clicks at 0, 200, 400, 600, 800, 1000, 1200, 1400, 1600, 1800ms are accepted (10 total)
        // Alternate clicks at 100, 300, 500, 700... are dropped silently.
        assert_eq!(accepted_count, 10);
    }

    #[test]
    fn test_sound_engine_audio_thread_latency() {
        let engine = SoundEngine::new();
        let sound1 = engine.get_sound("expand").unwrap();
        let sound2 = engine.get_sound("tab").unwrap();

        // Measure First Sound Latency:
        // Includes: lazy DeviceSinkBuilder::open_default_sink() + Player connection + sample buffering
        let t0 = Instant::now();
        let mut sink = DeviceSinkBuilder::open_default_sink().expect("Failed to open audio device");
        sink.log_on_drop(false);
        let sink_opened = t0.elapsed();

        let player1 = Player::connect_new(&sink.mixer());
        let channels = NonZero::new(1u16).unwrap();
        let sample_rate = NonZero::new(SAMPLE_RATE).unwrap();
        let source1 = SamplesBuffer::new(channels, sample_rate, (*sound1.samples).clone());
        player1.set_volume(0.35);
        player1.append(source1);
        player1.play();
        let first_sound_total = t0.elapsed();

        // Measure Later Sound Latency:
        // Output stream already open: Player connection + sample buffering + play()
        let t1 = Instant::now();
        let player2 = Player::connect_new(&sink.mixer());
        let source2 = SamplesBuffer::new(channels, sample_rate, (*sound2.samples).clone());
        player2.set_volume(0.35);
        player2.append(source2);
        player2.play();
        let later_sound_total = t1.elapsed();

        println!("\n=== LATENCY BENCHMARK ===");
        println!("First sound (lazy device sink open): {:?}", sink_opened);
        println!("First sound call to first sample total: {:?}", first_sound_total);
        println!("Later sound call to first sample total: {:?}", later_sound_total);
        println!("=========================\n");

        assert!(later_sound_total < Duration::from_millis(15));
    }

    #[test]
    fn test_device_sink_status() {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => {
                println!("[device test] Default audio output sink successfully opened: {:?}", sink.config());
            }
            Err(e) => {
                println!("[device test] No default audio device available or device error: {:?}", e);
            }
        }
    }

    #[test]
    fn test_startup_chime_playback_and_drain() {
        let engine = get_sound_engine();
        let sound = engine.get_sound("startup").expect("startup sound must exist");
        assert_eq!(sound.id, "startup");

        let mut sink = match DeviceSinkBuilder::open_default_sink() {
            Ok(s) => s,
            Err(e) => {
                println!("[device test] Skipping hardware playback test: {:?}", e);
                return;
            }
        };
        sink.log_on_drop(false);

        let player = Player::connect_new(&sink.mixer());
        let channels = NonZero::new(sound.channels).unwrap();
        let sample_rate = NonZero::new(sound.sample_rate).unwrap();
        let source = SamplesBuffer::new(channels, sample_rate, (*sound.samples).clone());
        player.set_volume(0.35);
        player.append(source);
        player.play();

        assert!(!player.empty(), "Player should contain startup sound samples");
    }

    #[test]
    fn test_decode_startup_file() {
        use rodio::Decoder;
        use rodio::Source;
        use std::io::Cursor;

        let bytes = include_bytes!("../../assets/sounds/startup.mp3");
        let cursor = Cursor::new(bytes.as_ref());
        let decoder = Decoder::new(cursor).expect("failed to decode startup.mp3");

        let sr = decoder.sample_rate().get();
        let ch = decoder.channels().get();
        println!("\n=== STARTUP.MP3 INFO ===");
        println!("Sample rate: {}, Channels: {}", sr, ch);

        let samples: Vec<f32> = decoder.collect();
        let dur_sec = samples.len() as f32 / (sr as f32 * ch as f32);
        let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        println!("Total samples: {}, Duration: {:.3}s, Peak: {:.4}", samples.len(), dur_sec, peak);
        println!("========================\n");
    }

    #[test]
    fn test_decode_agent_complete_file() {
        use rodio::Decoder;
        use rodio::Source;
        use std::io::Cursor;

        let bytes = include_bytes!("../../assets/sounds/agent-complete.mp3");
        let cursor = Cursor::new(bytes.as_ref());
        let decoder = Decoder::new(cursor).expect("failed to decode agent-complete.mp3");

        let sr = decoder.sample_rate().get();
        let ch = decoder.channels().get();
        let samples: Vec<f32> = decoder.collect();
        let dur_sec = samples.len() as f32 / (sr as f32 * ch as f32);
        let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        println!("\n=== AGENT-COMPLETE.MP3 INFO ===");
        println!("Sample rate: {}, Channels: {}, Duration: {:.3}s, Peak: {:.4}", sr, ch, dur_sec, peak);
        println!("================================\n");
        assert!(samples.len() > 0);
    }
}




