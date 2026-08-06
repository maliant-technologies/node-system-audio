//! Loopback capture.
//!
//! Build an input stream on the default *output* device. cpal picks the backend
//! mechanism: WASAPI `AUDCLNT_STREAMFLAGS_LOOPBACK`, a PipeWire/Pulse monitor
//! source, or a CoreAudio process tap.
//!
//! The capture callback only downmixes and hands off; resampling, allocation and
//! ring writes happen on the worker thread.
//!
//! The worker also supervises. On a stream fault or a default-device change it
//! reopens the stream, keeping the ring intact.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, DeviceId, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};

use crate::error::{Error, Result};
use crate::resample::Resampler;
use crate::ring::RetentionRing;

/// Worker wake interval. Short enough that a gap is detected promptly, long
/// enough that the thread is effectively idle.
const TICK: Duration = Duration::from_millis(20);

/// Grace before a quiet stream counts as a gap rather than jitter.
const GAP_TOLERANCE_SECS: f64 = 0.25;

/// Ceiling on silence synthesised per tick. Bounds the allocation after the
/// process is suspended; the loop runs 50x/sec so it still catches up quickly.
const MAX_SYNTH_SECS: f64 = 1.0;

/// How often to poll the host for a default-device change.
const DEVICE_CHECK: Duration = Duration::from_millis(500);

/// Backoff after a failed rebuild.
const REBUILD_BACKOFF: Duration = Duration::from_millis(750);

/// State shared between the capture callback, the worker, and the JS thread.
pub struct Shared {
    pub ring: Mutex<RetentionRing>,
    /// Native-rate mono, produced by the callback and drained by the worker.
    staging: Mutex<Vec<f32>>,
    pub silence_frames: AtomicU64,

    /// Latched: survives being read. Cleared by `clear_error`, `stop`, or a
    /// successful rebuild.
    last_error: Mutex<Option<String>>,
    /// Raised by the cpal error callback, consumed by the supervisor.
    faulted: AtomicBool,

    pub restarts: AtomicU32,
    pub native_rate: AtomicU32,
    pub channels: AtomicU32,
    /// Name of the device currently being captured.
    pub device_name: Mutex<String>,
}

impl Shared {
    pub fn new(ring: RetentionRing) -> Self {
        Self {
            ring: Mutex::new(ring),
            staging: Mutex::new(Vec::new()),
            silence_frames: AtomicU64::new(0),
            last_error: Mutex::new(None),
            faulted: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
            native_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            device_name: Mutex::new(String::new()),
        }
    }

    /// The latched error. Reading does not clear it.
    pub fn error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }

    pub fn clear_error(&self) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = None;
        }
        self.faulted.store(false, Ordering::Relaxed);
    }

    fn record_error(&self, message: String) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message);
        }
    }

    pub fn device_name(&self) -> Option<String> {
        let name = self.device_name.lock().ok()?.clone();
        (!name.is_empty()).then_some(name)
    }
}

/// A running capture. Dropping it stops the stream and joins the worker.
pub struct Capture {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Capture {
    /// Open the default output device and start the supervisor.
    ///
    /// # Errors
    ///
    /// No default output device, unreadable config, or the stream failing to
    /// open or start. On macOS a denied permission is not an error here: the
    /// stream opens and delivers no frames.
    pub fn start(shared: Arc<Shared>) -> Result<Self> {
        let host = cpal::default_host();
        let open = OpenStream::open(&host, &shared)?;

        let target_rate = shared.ring.lock().map_err(|_| Error::Poisoned)?.rate();

        let resampler = Resampler::new(open.native_rate, target_rate)?;
        let stop = Arc::new(AtomicBool::new(false));

        let worker = spawn_supervisor(
            Arc::clone(&shared),
            Arc::clone(&stop),
            open,
            resampler,
            target_rate,
        );

        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // Nothing useful to do with a panicked worker during drop.
            let _ = worker.join();
        }
    }
}

/// An opened stream plus the facts about the device behind it.
struct OpenStream {
    stream: Stream,
    device_id: Option<DeviceId>,
    native_rate: u32,
}

impl OpenStream {
    fn open(host: &cpal::Host, shared: &Arc<Shared>) -> Result<Self> {
        // Output device, not input: that is how loopback works.
        let device = host.default_output_device().ok_or(Error::NoOutputDevice)?;

        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let supported = device
            .default_output_config()
            .map_err(Error::DeviceConfig)?;

        // `SampleRate` is a plain u32 in cpal 0.18: no newtype to unwrap.
        let native_rate = supported.sample_rate();
        let channels = supported.channels();
        let format = supported.sample_format();
        let config = supported.config();

        let stream = build_stream(&device, format, config, channels, &name, Arc::clone(shared))?;
        stream.play().map_err(Error::StartStream)?;

        // Best-effort. Without an id we lose change detection but still capture.
        let device_id = device.id().ok();

        shared.native_rate.store(native_rate, Ordering::Relaxed);
        shared
            .channels
            .store(u32::from(channels), Ordering::Relaxed);
        if let Ok(mut slot) = shared.device_name.lock() {
            *slot = name;
        }

        Ok(Self {
            stream,
            device_id,
            native_rate,
        })
    }
}

/// Frame accounting for one device session. Reset on device change, since
/// counts at 44.1 kHz mean nothing once a 48 kHz device takes over.
struct Epoch {
    started: Instant,
    /// Frames received plus frames synthesised, at `rate`.
    accounted: u64,
    rate: u32,
}

impl Epoch {
    fn new(rate: u32) -> Self {
        Self {
            started: Instant::now(),
            accounted: 0,
            rate,
        }
    }

    /// Frames the wall clock says should have arrived but haven't.
    fn deficit(&self) -> f64 {
        let elapsed = self.started.elapsed().as_secs_f64() * f64::from(self.rate);
        elapsed - self.accounted as f64 - f64::from(self.rate) * GAP_TOLERANCE_SECS
    }
}

fn spawn_supervisor(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    open: OpenStream,
    mut resampler: Resampler,
    target_rate: u32,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let host = cpal::default_host();

        let OpenStream {
            mut stream,
            mut device_id,
            mut native_rate,
        } = open;

        let mut epoch = Epoch::new(native_rate);
        let mut silence = vec![0.0f32; (f64::from(native_rate) * MAX_SYNTH_SECS) as usize];
        let mut out: Vec<i16> = Vec::with_capacity(target_rate as usize);
        let mut last_device_check = Instant::now();
        let mut reported_drops = 0u64;

        while !stop.load(Ordering::Relaxed) {
            thread::sleep(TICK);

            let drained = match shared.staging.lock() {
                Ok(mut staged) => std::mem::take(&mut *staged),
                // The callback panicked; stop buffering.
                Err(_) => {
                    shared.record_error("capture thread panicked".to_string());
                    break;
                }
            };

            if !drained.is_empty() {
                epoch.accounted += drained.len() as u64;
                out.extend(resampler.push(&drained));
            }

            // WASAPI delivers nothing while the device is silent, so the ring
            // would drift from the wall clock. Fill the shortfall. Also covers
            // the window between a device going away and its replacement.
            let deficit = epoch.deficit();
            if deficit >= 1.0 {
                let n = (deficit as usize).min(silence.len());
                epoch.accounted += n as u64;
                shared.silence_frames.fetch_add(n as u64, Ordering::Relaxed);
                out.extend(resampler.push(&silence[..n]));
            }

            if !out.is_empty() {
                match shared.ring.lock() {
                    Ok(mut ring) => ring.push(&out),
                    Err(_) => {
                        shared.record_error("audio buffer was poisoned".to_string());
                        break;
                    }
                }
                out.clear();
            }

            // Refused chunks make the output shorter than elapsed time, which
            // looks like a capture fault. Report it.
            let dropped = resampler.dropped_chunks();
            if dropped > reported_drops {
                reported_drops = dropped;
                shared.record_error(format!(
                    "resampler rejected {dropped} chunk(s); some audio is missing"
                ));
            }

            // ---- supervision ------------------------------------------------

            let faulted = shared.faulted.swap(false, Ordering::Relaxed);
            let device_changed = if last_device_check.elapsed() >= DEVICE_CHECK {
                last_device_check = Instant::now();
                default_device_changed(&host, device_id.as_ref())
            } else {
                false
            };

            if !faulted && !device_changed {
                continue;
            }

            // Some backends refuse a second capture on the same device.
            drop(stream);

            match OpenStream::open(&host, &shared) {
                Ok(reopened) => {
                    if reopened.native_rate != native_rate {
                        match Resampler::new(reopened.native_rate, target_rate) {
                            Ok(fresh) => resampler = fresh,
                            Err(e) => {
                                // Unusable device; stop rather than fill the
                                // ring with silence indefinitely.
                                shared.record_error(e.to_string());
                                return;
                            }
                        }
                        native_rate = reopened.native_rate;
                        silence = vec![0.0f32; (f64::from(native_rate) * MAX_SYNTH_SECS) as usize];
                    }

                    stream = reopened.stream;
                    device_id = reopened.device_id;
                    epoch = Epoch::new(native_rate);
                    shared.restarts.fetch_add(1, Ordering::Relaxed);
                    shared.clear_error();
                }
                Err(e) => {
                    shared.record_error(e.to_string());
                    thread::sleep(REBUILD_BACKOFF);

                    // Retry once; the gap filler covers the interval.
                    match OpenStream::open(&host, &shared) {
                        Ok(reopened) => {
                            stream = reopened.stream;
                            device_id = reopened.device_id;
                            native_rate = reopened.native_rate;
                            epoch = Epoch::new(native_rate);
                            shared.restarts.fetch_add(1, Ordering::Relaxed);
                            shared.clear_error();
                        }
                        Err(again) => {
                            shared.record_error(again.to_string());
                            return;
                        }
                    }
                }
            }
        }
    })
}

/// Whether the default output device differs from the one being captured.
/// An unidentifiable device counts as unchanged, to avoid reopening every tick.
fn default_device_changed(host: &cpal::Host, current: Option<&DeviceId>) -> bool {
    let Some(current) = current else {
        return false;
    };
    match host.default_output_device() {
        Some(device) => device.id().map(|id| &id != current).unwrap_or(false),
        // Nothing to reopen onto; the gap filler covers it until one returns.
        None => false,
    }
}

fn build_stream(
    device: &Device,
    format: SampleFormat,
    config: StreamConfig,
    channels: u16,
    device_name: &str,
    shared: Arc<Shared>,
) -> Result<Stream> {
    match format {
        SampleFormat::F32 => typed::<f32>(device, config, channels, device_name, shared),
        SampleFormat::F64 => typed::<f64>(device, config, channels, device_name, shared),
        SampleFormat::I16 => typed::<i16>(device, config, channels, device_name, shared),
        SampleFormat::I32 => typed::<i32>(device, config, channels, device_name, shared),
        SampleFormat::I8 => typed::<i8>(device, config, channels, device_name, shared),
        SampleFormat::U8 => typed::<u8>(device, config, channels, device_name, shared),
        SampleFormat::U16 => typed::<u16>(device, config, channels, device_name, shared),
        SampleFormat::U32 => typed::<u32>(device, config, channels, device_name, shared),
        other => Err(Error::UnsupportedSampleFormat(other)),
    }
}

fn typed<T>(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    device_name: &str,
    shared: Arc<Shared>,
) -> Result<Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channel_count = usize::from(channels.max(1));
    let err_shared = Arc::clone(&shared);

    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                // Downmix to mono and hand off. Nothing heavier on this thread.
                let Ok(mut staged) = shared.staging.lock() else {
                    return;
                };
                staged.reserve(data.len() / channel_count);
                for frame in data.chunks_exact(channel_count) {
                    let sum: f32 = frame.iter().copied().map(f32::from_sample_).sum();
                    staged.push(sum / channel_count as f32);
                }
            },
            move |e| {
                // Flag only; the supervisor handles it. Must not block.
                err_shared.record_error(e.to_string());
                err_shared.faulted.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|source| Error::OpenStream {
            device: device_name.to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_latch() {
        let shared = Shared::new(RetentionRing::new(16_000, 1_000, 1_000));

        shared.record_error("device exploded".to_string());

        // Reading must not consume it: a UI polling every 200ms would see the
        // error for one frame and then lose it.
        assert_eq!(shared.error().as_deref(), Some("device exploded"));
        assert_eq!(shared.error().as_deref(), Some("device exploded"));

        shared.clear_error();
        assert_eq!(shared.error(), None);
    }

    #[test]
    fn clear_error_lowers_fault_flag() {
        let shared = Shared::new(RetentionRing::new(16_000, 1_000, 1_000));
        shared.faulted.store(true, Ordering::Relaxed);

        shared.clear_error();

        assert!(!shared.faulted.load(Ordering::Relaxed));
    }

    #[test]
    fn fault_consumed_once() {
        let shared = Shared::new(RetentionRing::new(16_000, 1_000, 1_000));
        shared.faulted.store(true, Ordering::Relaxed);

        // Swap, not load, so one fault triggers one rebuild.
        assert!(shared.faulted.swap(false, Ordering::Relaxed));
        assert!(!shared.faulted.swap(false, Ordering::Relaxed));
    }

    #[test]
    fn device_name_absent_until_opened() {
        let shared = Shared::new(RetentionRing::new(16_000, 1_000, 1_000));
        assert_eq!(shared.device_name(), None);

        *shared.device_name.lock().expect("fresh mutex") = "Speakers".to_string();
        assert_eq!(shared.device_name().as_deref(), Some("Speakers"));
    }

    #[test]
    fn no_deficit_within_tolerance() {
        let epoch = Epoch::new(48_000);
        // Nothing accounted, but 250ms has not passed either.
        assert!(epoch.deficit() < 0.0);
    }

    #[test]
    fn deficit_grows_when_frames_stop() {
        let mut epoch = Epoch::new(48_000);
        epoch.started = Instant::now() - Duration::from_secs(2);

        // 2s elapsed, nothing received, 0.25s tolerance => ~1.75s of silence.
        let deficit = epoch.deficit();
        assert!(
            (deficit - 48_000.0 * 1.75).abs() < 48_000.0 * 0.1,
            "expected ~84000 frames of deficit, got {deficit}"
        );
    }

    #[test]
    fn accounted_frames_cancel_deficit() {
        let mut epoch = Epoch::new(48_000);
        epoch.started = Instant::now() - Duration::from_secs(2);
        epoch.accounted = 48_000 * 2;

        assert!(
            epoch.deficit() < 0.0,
            "a stream delivering on time should never trigger the gap filler"
        );
    }

    #[test]
    fn unidentifiable_device_never_changed() {
        assert!(!default_device_changed(&cpal::default_host(), None));
    }
}
