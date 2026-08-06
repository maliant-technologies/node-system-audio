//! Cross-platform system audio (loopback) capture with a rolling retention
//! buffer, as a Node native addon.
//!
//! The buffer stays in Rust. 15 minutes of mono 16 kHz is ~28 MB that never
//! crosses into V8; JS asks for a slice when it needs one.

#![deny(clippy::correctness)]
#![warn(clippy::suspicious, clippy::complexity, clippy::perf)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod capture;
mod error;
mod permission;
mod resample;
mod ring;
mod wav;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Float32Array};
use napi::Result;
use napi_derive::napi;

use capture::{Capture, Shared};
use error::Error;
use ring::{RetentionRing, DEFAULT_CEILING_SECS};

const DEFAULT_TARGET_RATE: u32 = 16_000;
const DEFAULT_RETENTION_SECS: u32 = 15 * 60;

#[napi(object)]
pub struct BufferOptions {
    /// How much history to keep. Defaults to 900 (15 minutes).
    pub retention_seconds: Option<u32>,
    /// Output rate. Defaults to 16000, which is what Whisper-style models want.
    pub target_sample_rate: Option<u32>,
    /// Allocation ceiling. `retentionSeconds` can be raised up to this at
    /// runtime without reallocating. Defaults to 3600.
    pub ceiling_seconds: Option<u32>,
}

#[napi(string_enum = "camelCase")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferState {
    /// Not capturing. Nothing is being recorded.
    Idle,
    /// Capturing into the rolling window.
    Running,
    /// Capture stopped, contents retained for reading.
    Frozen,
}

#[napi(string_enum = "camelCase")]
pub enum PermissionStatus {
    Granted,
    Denied,
    /// macOS without preflight: knowable only by attempting capture.
    Unknown,
    /// Windows and Linux: loopback needs no permission.
    NotRequired,
}

#[napi(object)]
pub struct BufferStatus {
    pub state: BufferState,
    /// Audio currently held, in milliseconds. Wall-clock accurate: gaps in the
    /// device's delivery are filled with silence rather than closed up.
    pub filled_ms: u32,
    /// The active retention window, in milliseconds.
    pub retention_ms: u32,
    /// Output rate of everything `read()` returns.
    pub sample_rate: u32,
    /// How much of the buffer is synthesised silence standing in for a gap in
    /// device delivery. Nonzero is normal on Windows during quiet passages.
    pub silence_inserted_ms: u32,
    /// The device rate being captured, once running. Can change mid-capture if
    /// the default output device changes.
    pub native_sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub device_name: Option<String>,
    /// How many times the stream has been rebuilt after a device fault or
    /// change. Recording continues across these; the buffer is preserved.
    pub restarts: u32,
    /// The last error, **latched**. Persists until `clearError()` or a
    /// successful automatic recovery, so polling cannot miss it.
    pub error: Option<String>,
}

#[napi]
pub struct SystemAudioBuffer {
    shared: Arc<Shared>,
    capture: Option<Capture>,
    state: BufferState,
    target_rate: u32,
}

#[napi]
impl SystemAudioBuffer {
    #[napi(constructor)]
    pub fn new(options: Option<BufferOptions>) -> Result<Self> {
        let options = options.unwrap_or(BufferOptions {
            retention_seconds: None,
            target_sample_rate: None,
            ceiling_seconds: None,
        });

        let target_rate = options.target_sample_rate.unwrap_or(DEFAULT_TARGET_RATE);
        if target_rate == 0 {
            return Err(Error::NotPositive("targetSampleRate").into());
        }

        let retention_secs = options.retention_seconds.unwrap_or(DEFAULT_RETENTION_SECS);
        if retention_secs == 0 {
            return Err(Error::NotPositive("retentionSeconds").into());
        }

        let ceiling_secs = options
            .ceiling_seconds
            .unwrap_or(DEFAULT_CEILING_SECS)
            .max(retention_secs);

        let ring = RetentionRing::new(
            target_rate,
            (ceiling_secs as usize) * target_rate as usize,
            (retention_secs as usize) * target_rate as usize,
        );

        Ok(Self {
            shared: Arc::new(Shared::new(ring)),
            capture: None,
            state: BufferState::Idle,
            target_rate,
        })
    }

    /// Begin capturing. Nothing records until this is called.
    ///
    /// On macOS this triggers the System Audio Recording prompt. A denial
    /// resolves successfully and accumulates no audio, so poll
    /// `status().filledMs` for a second to tell the difference.
    #[napi]
    pub fn start(&mut self) -> Result<()> {
        match self.state {
            BufferState::Running => return Ok(()),
            BufferState::Frozen => return Err(Error::Frozen.into()),
            BufferState::Idle => {}
        }

        let capture = Capture::start(Arc::clone(&self.shared))?;
        self.capture = Some(capture);
        self.state = BufferState::Running;
        Ok(())
    }

    /// Stop capturing and discard the contents.
    #[napi]
    pub fn stop(&mut self) -> Result<()> {
        self.capture = None;
        self.state = BufferState::Idle;
        self.shared.silence_frames.store(0, Ordering::Relaxed);
        self.shared
            .ring
            .lock()
            .map_err(|_| Error::Poisoned)?
            .clear();
        Ok(())
    }

    /// Stop capturing but keep the contents, for examining and cutting.
    ///
    /// Audio playing during a freeze is not recorded.
    #[napi]
    pub fn freeze(&mut self) -> Result<()> {
        match self.state {
            BufferState::Running => {}
            BufferState::Frozen => return Ok(()),
            BufferState::Idle => return Err(Error::NotRunning.into()),
        }

        // Dropping the capture stops the stream and joins the worker.
        self.capture = None;
        self.state = BufferState::Frozen;
        Ok(())
    }

    /// Resume capturing after a freeze, starting from empty.
    ///
    /// Contents are cleared rather than appended to: keeping them would leave an
    /// invisible discontinuity mid-window, so every position in a waveform drawn
    /// over it would be wrong.
    #[napi]
    pub fn resume(&mut self) -> Result<()> {
        match self.state {
            BufferState::Frozen => {}
            BufferState::Running => return Ok(()),
            BufferState::Idle => return Err(Error::NotFrozen.into()),
        }

        self.shared.silence_frames.store(0, Ordering::Relaxed);
        self.shared
            .ring
            .lock()
            .map_err(|_| Error::Poisoned)?
            .clear();

        let capture = Capture::start(Arc::clone(&self.shared))?;
        self.capture = Some(capture);
        self.state = BufferState::Running;
        Ok(())
    }

    /// A snapshot of what the buffer is doing.
    ///
    /// Device facts come from the supervisor, not from `start()`, since the
    /// device can change underneath a running capture.
    ///
    /// # Errors
    ///
    /// If a thread panicked while holding the buffer.
    #[napi]
    pub fn status(&self) -> Result<BufferStatus> {
        let ring = self.shared.ring.lock().map_err(|_| Error::Poisoned)?;

        let silence_frames = self.shared.silence_frames.load(Ordering::Relaxed);
        let silence_inserted_ms = ((silence_frames * 1000) / u64::from(self.target_rate.max(1)))
            .min(u64::from(u32::MAX)) as u32;

        let running = !matches!(self.state, BufferState::Idle);
        let non_zero = |v: u32| (v != 0).then_some(v);

        Ok(BufferStatus {
            state: self.state,
            filled_ms: ring.window_ms(),
            retention_ms: ((ring.retention_frames() as u64 * 1000)
                / u64::from(self.target_rate.max(1))) as u32,
            sample_rate: ring.rate(),
            silence_inserted_ms,
            native_sample_rate: running
                .then(|| non_zero(self.shared.native_rate.load(Ordering::Relaxed)))
                .flatten(),
            channels: running
                .then(|| non_zero(self.shared.channels.load(Ordering::Relaxed)))
                .flatten(),
            device_name: running.then(|| self.shared.device_name()).flatten(),
            restarts: self.shared.restarts.load(Ordering::Relaxed),
            error: self.shared.error(),
        })
    }

    /// Dismiss a latched error. A successful automatic recovery clears it too.
    #[napi]
    pub fn clear_error(&self) {
        self.shared.clear_error();
    }

    /// Change how much history to keep, without reallocating.
    ///
    /// Lowering it hides older audio; raising it back reveals whatever is still
    /// physically present, up to the ceiling set at construction.
    #[napi]
    pub fn set_retention_seconds(&mut self, seconds: u32) -> Result<()> {
        if seconds == 0 {
            return Err(Error::NotPositive("retentionSeconds").into());
        }
        self.shared
            .ring
            .lock()
            .map_err(|_| Error::Poisoned)?
            .set_retention_frames(seconds as usize * self.target_rate as usize);
        Ok(())
    }

    /// Cut a range out as a mono 16-bit WAV.
    ///
    /// Offsets are milliseconds from the oldest audio held. Out-of-range values
    /// clamp. A container rather than raw PCM so format sniffers accept it and
    /// the bytes play in an `<audio>` element unchanged.
    #[napi]
    pub fn read(&self, start_ms: u32, end_ms: u32) -> Result<Buffer> {
        let ring = self.shared.ring.lock().map_err(|_| Error::Poisoned)?;

        let samples = ring.read_frames(ring.ms_to_frames(start_ms), ring.ms_to_frames(end_ms));
        Ok(wav::mono_pcm16(&samples, ring.rate()).into())
    }

    /// Waveform envelope: `[min, max]` per bucket, interleaved and normalised to
    /// -1.0..=1.0, so the array is `buckets * 2` long. Computed here so drawing
    /// costs a few thousand floats rather than the whole buffer.
    #[napi]
    pub fn peaks(&self, buckets: u32) -> Result<Float32Array> {
        if buckets == 0 {
            return Err(Error::NotPositive("buckets").into());
        }
        let ring = self.shared.ring.lock().map_err(|_| Error::Poisoned)?;
        Ok(Float32Array::new(ring.peaks(buckets as usize)))
    }

    /// Whether the OS will allow system audio capture.
    ///
    /// `notRequired` on Windows and Linux; `unknown` on macOS unless built with
    /// the `tcc-preflight` feature.
    #[napi]
    pub fn permission_status(&self) -> PermissionStatus {
        current_permission_status()
    }
}

/// Whether this platform needs a permission grant for loopback capture, without
/// constructing a buffer first.
#[napi]
pub fn permission_status() -> PermissionStatus {
    current_permission_status()
}

fn current_permission_status() -> PermissionStatus {
    match permission::status() {
        permission::Status::Granted => PermissionStatus::Granted,
        permission::Status::Denied => PermissionStatus::Denied,
        permission::Status::Unknown => PermissionStatus::Unknown,
        permission::Status::NotRequired => PermissionStatus::NotRequired,
    }
}
