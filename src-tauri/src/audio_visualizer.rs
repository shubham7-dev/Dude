// audio_visualizer.rs - Real-time WASAPI Loopback Audio Spectrum Capture for Now Playing bars

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

const FFT_SIZE: usize = 512;
const NUM_BARS: usize = 14;

// Global visualizer singleton
static VISUALIZER: OnceLock<Mutex<AudioVisualizerState>> = OnceLock::new();

pub struct AudioVisualizerState {
    running: Arc<AtomicBool>,
}

impl AudioVisualizerState {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn get_visualizer() -> &'static Mutex<AudioVisualizerState> {
    VISUALIZER.get_or_init(|| Mutex::new(AudioVisualizerState::new()))
}

#[tauri::command]
pub fn start_audio_visualizer(app_handle: AppHandle) {
    let state = get_visualizer().lock().unwrap();
    if state.running.load(Ordering::SeqCst) {
        return;
    }

    state.running.store(true, Ordering::SeqCst);
    let running_clone = state.running.clone();

    // Spawn non-blocking background thread. Notice: no thread handle stored, no h.join() deadlock!
    let _ = thread::Builder::new()
        .name("dude-audio-spectrum".to_string())
        .spawn(move || {
            struct AutoReset(Arc<AtomicBool>);
            impl Drop for AutoReset {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _guard = AutoReset(running_clone.clone());
            run_wasapi_loopback_capture(running_clone, app_handle);
        });
}

#[tauri::command]
pub fn stop_audio_visualizer() {
    let state = get_visualizer().lock().unwrap();
    // Setting atomic flag is instantaneous and never blocks the GUI thread or deadlocks
    state.running.store(false, Ordering::SeqCst);
}

pub fn is_visualizer_running() -> bool {
    let state = get_visualizer().lock().unwrap();
    state.running.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// 512-Point Radix-2 In-Place Fast Fourier Transform & Frequency Band Grouping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    #[inline(always)]
    fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    #[inline(always)]
    fn add(&self, other: &Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }

    #[inline(always)]
    fn sub(&self, other: &Self) -> Self {
        Self {
            re: self.re - other.re,
            im: self.im - other.im,
        }
    }

    #[inline(always)]
    fn mul(&self, other: &Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }

    #[inline(always)]
    fn norm(&self) -> f32 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
}

fn fft_512(data: &mut [Complex; FFT_SIZE]) {
    let mut j = 0;
    for i in 0..(FFT_SIZE - 1) {
        if i < j {
            data.swap(i, j);
        }
        let mut bit = FFT_SIZE >> 1;
        while bit > 0 && bit <= j {
            j -= bit;
            bit >>= 1;
        }
        j += bit;
    }

    let mut len = 2;
    while len <= FFT_SIZE {
        let half = len / 2;
        let angle = -2.0 * PI / (len as f32);
        let w_step = Complex::new(angle.cos(), angle.sin());

        let mut i = 0;
        while i < FFT_SIZE {
            let mut w = Complex::new(1.0, 0.0);
            for k in 0..half {
                let u = data[i + k];
                let v = data[i + k + half].mul(&w);
                data[i + k] = u.add(&v);
                data[i + k + half] = u.sub(&v);
                w = w.mul(&w_step);
            }
            i += len;
        }
        len <<= 1;
    }
}

const BAND_RANGES: [(usize, usize); NUM_BARS] = [
    (1, 2),     // Band 0:  ~94 - 188 Hz (Sub-bass)
    (2, 3),     // Band 1:  ~188 - 281 Hz (Bass punch)
    (3, 4),     // Band 2:  ~281 - 375 Hz (Upper bass)
    (4, 6),     // Band 3:  ~375 - 562 Hz (Low mids)
    (6, 8),     // Band 4:  ~562 - 750 Hz (Warmth)
    (8, 11),    // Band 5:  ~750 - 1030 Hz (Midrange 1)
    (11, 15),   // Band 6:  ~1030 - 1400 Hz (Midrange 2)
    (15, 21),   // Band 7:  ~1400 - 1970 Hz (Upper mids)
    (21, 29),   // Band 8:  ~1970 - 2720 Hz (Attack)
    (29, 40),   // Band 9:  ~2720 - 3750 Hz (Presence)
    (40, 56),   // Band 10: ~3750 - 5250 Hz (High presence)
    (56, 78),   // Band 11: ~5250 - 7300 Hz (Crispness)
    (78, 110),  // Band 12: ~7300 - 10300 Hz (Treble)
    (110, 160), // Band 13: ~10300 - 15000 Hz (Air)
];

const BAND_EQ: [f32; NUM_BARS] = [
    1.00, 1.05, 1.10, 1.15, 1.25, 1.40, 1.50, 1.65, 1.80, 2.05, 2.30, 2.65, 3.10, 3.60,
];

fn get_hann_window() -> [f32; FFT_SIZE] {
    let mut win = [0.0f32; FFT_SIZE];
    for (i, w) in win.iter_mut().enumerate() {
        *w = 0.5 * (1.0 - (2.0 * PI * (i as f32) / (FFT_SIZE as f32)).cos());
    }
    win
}

// ---------------------------------------------------------------------------
// WASAPI Loopback Capture Loop (Safe, bounded, zero-freeze)
// ---------------------------------------------------------------------------

fn run_wasapi_loopback_capture(running: Arc<AtomicBool>, app_handle: AppHandle) {
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            eprintln!("[AudioVisualizer] COM init failed");
            return;
        }
    }

    capture_worker_loop(&running, &app_handle);

    unsafe {
        CoUninitialize();
    }
}

fn capture_worker_loop(running: &Arc<AtomicBool>, app_handle: &AppHandle) {
    unsafe {
        let enumerator: IMMDeviceEnumerator = match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("[AudioVisualizer] CoCreateInstance failed: {:?}", err);
                return;
            }
        };

        let device = match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
            Ok(d) => d,
            Err(err) => {
                eprintln!("[AudioVisualizer] GetDefaultAudioEndpoint failed: {:?}", err);
                return;
            }
        };

        let client: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
            Ok(c) => c,
            Err(err) => {
                eprintln!("[AudioVisualizer] Activate failed: {:?}", err);
                return;
            }
        };

        let pwfx = match client.GetMixFormat() {
            Ok(f) => f,
            Err(err) => {
                eprintln!("[AudioVisualizer] GetMixFormat failed: {:?}", err);
                return;
            }
        };

        let wfx = std::ptr::read_unaligned(pwfx);
        let channels = wfx.nChannels as usize;
        let bits_per_sample = wfx.wBitsPerSample as usize;
        let format_tag = wfx.wFormatTag as u32;

        let is_float = if format_tag == 3 {
            true
        } else if format_tag == 0xFFFE {
            let ext = std::ptr::read_unaligned(pwfx as *const WAVEFORMATEXTENSIBLE);
            ext.SubFormat.data1 == 3 || bits_per_sample == 32
        } else {
            bits_per_sample == 32
        };

        let init_res = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000,
            0,
            pwfx,
            None,
        );

        CoTaskMemFree(Some(pwfx as *const _ as *const std::ffi::c_void));

        if let Err(err) = init_res {
            eprintln!("[AudioVisualizer] Initialize failed: {:?}", err);
            return;
        }

        let capture: IAudioCaptureClient = match client.GetService() {
            Ok(c) => c,
            Err(err) => {
                eprintln!("[AudioVisualizer] GetService failed: {:?}", err);
                return;
            }
        };

        if let Err(err) = client.Start() {
            eprintln!("[AudioVisualizer] Start failed: {:?}", err);
            return;
        }

        let mut sample_ring = [0.0f32; FFT_SIZE];
        let mut ring_idx = 0usize;
        let hann_window = get_hann_window();
        let mut smoothed_bars = [0.08f32; NUM_BARS];

        // While running, loop with mandatory sleep on EVERY iteration
        while running.load(Ordering::Relaxed) {
            // ALWAYS sleep 25ms between iterations -> ~40 FPS, maximum ~0.1% CPU!
            thread::sleep(Duration::from_millis(25));

            if !running.load(Ordering::Relaxed) {
                break;
            }

            // Read available packets up to a STRICT maximum of 8 packets to prevent any spin
            let mut packets_read = 0;
            while packets_read < 8 {
                let packet_size = match capture.GetNextPacketSize() {
                    Ok(ps) => ps,
                    Err(_) => break,
                };

                if packet_size == 0 {
                    break;
                }
                packets_read += 1;

                let mut p_data: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                if capture
                    .GetBuffer(&mut p_data, &mut num_frames, &mut flags, None, None)
                    .is_ok()
                {
                    let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                    if !is_silent && !p_data.is_null() && num_frames > 0 && channels > 0 {
                        if is_float && bits_per_sample == 32 {
                            let floats = std::slice::from_raw_parts(
                                p_data as *const f32,
                                (num_frames as usize) * channels,
                            );

                            for f in 0..(num_frames as usize) {
                                let mut sum = 0.0f32;
                                for c in 0..channels {
                                    sum += floats[f * channels + c];
                                }
                                sample_ring[ring_idx] = sum / (channels as f32);
                                ring_idx = (ring_idx + 1) % FFT_SIZE;
                            }
                        } else if bits_per_sample == 16 {
                            let shorts = std::slice::from_raw_parts(
                                p_data as *const i16,
                                (num_frames as usize) * channels,
                            );

                            for f in 0..(num_frames as usize) {
                                let mut sum = 0.0f32;
                                for c in 0..channels {
                                    sum += shorts[f * channels + c] as f32 / 32768.0;
                                }
                                sample_ring[ring_idx] = sum / (channels as f32);
                                ring_idx = (ring_idx + 1) % FFT_SIZE;
                            }
                        }
                    }

                    let _ = capture.ReleaseBuffer(num_frames);
                } else {
                    break;
                }
            }

            if packets_read == 0 {
                // If no audio packets arrived during this tick, decay sample_ring towards silence
                for s in sample_ring.iter_mut() {
                    *s *= 0.80;
                }
            }

            // FFT and spectrum calculation
            let mut fft_input: [Complex; FFT_SIZE] = std::array::from_fn(|i| {
                let s_idx = (ring_idx + i) % FFT_SIZE;
                let val = sample_ring[s_idx] * hann_window[i];
                Complex::new(val, 0.0)
            });

            fft_512(&mut fft_input);

            let mut raw_bands = [0.0f32; NUM_BARS];
            for (b_idx, &(start_bin, end_bin)) in BAND_RANGES.iter().enumerate() {
                let mut max_mag = 0.0f32;
                let mut sum_mag = 0.0f32;
                let count = (end_bin - start_bin).max(1) as f32;

                for k in start_bin..end_bin {
                    let norm = fft_input[k].norm() / 256.0;
                    if norm > max_mag {
                        max_mag = norm;
                    }
                    sum_mag += norm;
                }

                let blended = (max_mag * 0.7 + (sum_mag / count) * 0.3) * BAND_EQ[b_idx];
                let scaled = (blended * 4.2).powf(0.55).min(1.0);
                raw_bands[b_idx] = scaled;
            }

            for i in 0..NUM_BARS {
                let target = raw_bands[i];
                if target > smoothed_bars[i] {
                    // Fast attack on beats
                    smoothed_bars[i] = smoothed_bars[i] * 0.28 + target * 0.72;
                } else {
                    // Musical decay
                    smoothed_bars[i] = smoothed_bars[i] * 0.74 + target * 0.26;
                }
                smoothed_bars[i] = smoothed_bars[i].clamp(0.08, 1.0);
            }

            // Emit to frontend (safe, rate-limited ~40 FPS)
            let _ = app_handle.emit("now-playing-bars", smoothed_bars.to_vec());
        }

        let _ = client.Stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fft_computation() {
        let mut data: [Complex; FFT_SIZE] = std::array::from_fn(|i| {
            let angle = 2.0 * PI * 10.0 * (i as f32) / (FFT_SIZE as f32);
            Complex::new(angle.sin(), 0.0)
        });
        fft_512(&mut data);
        let peak_bin_10 = data[10].norm();
        let dc = data[0].norm();
        assert!(peak_bin_10 > 100.0, "FFT bin 10 should detect sine wave");
        assert!(dc < 1.0, "DC component should be near zero");
    }

    #[test]
    fn test_wasapi_device_format() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator, _> = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            if let Ok(enumerator) = enumerator {
                if let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                    if let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) {
                        if let Ok(pwfx) = client.GetMixFormat() {
                            let wfx = std::ptr::read_unaligned(pwfx);
                            let ch = wfx.nChannels;
                            let sr = wfx.nSamplesPerSec;
                            let bits = wfx.wBitsPerSample;
                            println!("[WASAPI Test] channels={}, sample_rate={}, bits={}", ch, sr, bits);
                            CoTaskMemFree(Some(pwfx as *const _ as *const std::ffi::c_void));
                        }
                    }
                }
            }
            CoUninitialize();
        }
    }
}
