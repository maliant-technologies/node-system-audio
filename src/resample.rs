//! Native-rate mono `f32` in, target-rate mono `i16` out.
//!
//! cpal delivers whatever the device runs at, usually 48 kHz; Whisper-style
//! models want 16 kHz. Runs on the worker thread, never in the callback.

use rubato::{FftFixedIn, Resampler as _};

use crate::error::{Error, Result};

/// Requested input frames per call. `FftFixedIn` rounds it to fit the ratio.
const CHUNK_IN: usize = 2048;
const SUB_CHUNKS: usize = 2;

pub struct Resampler {
    /// `None` when the device already runs at the target rate.
    inner: Option<FftFixedIn<f32>>,
    /// Input frames the resampler wants next.
    chunk_in: usize,
    /// Input carried over from the last call.
    pending: Vec<f32>,
    scratch_in: Vec<Vec<f32>>,
    /// Chunks rubato refused. Counted so a failing resampler is visible rather
    /// than showing up as unexplained short audio.
    dropped_chunks: u64,
}

impl Resampler {
    /// # Errors
    ///
    /// Returns [`Error::ZeroSampleRate`] if either rate is zero, or
    /// [`Error::Resampler`] if rubato rejects the ratio.
    pub fn new(in_rate: u32, out_rate: u32) -> Result<Self> {
        if in_rate == 0 || out_rate == 0 {
            return Err(Error::ZeroSampleRate);
        }

        if in_rate == out_rate {
            return Ok(Self {
                inner: None,
                chunk_in: CHUNK_IN,
                pending: Vec::new(),
                scratch_in: Vec::new(),
                dropped_chunks: 0,
            });
        }

        let inner =
            FftFixedIn::<f32>::new(in_rate as usize, out_rate as usize, CHUNK_IN, SUB_CHUNKS, 1)
                .map_err(|source| Error::Resampler {
                    from: in_rate,
                    to: out_rate,
                    source,
                })?;
        let chunk_in = inner.input_frames_next();

        Ok(Self {
            inner: Some(inner),
            chunk_in,
            pending: Vec::with_capacity(chunk_in * 2),
            scratch_in: vec![Vec::with_capacity(chunk_in)],
            dropped_chunks: 0,
        })
    }

    /// Feed native-rate mono samples; get back however many target-rate samples
    /// are ready. Input that doesn't fill a whole chunk is held for next time,
    /// so calling this with tiny buffers is fine.
    pub fn push(&mut self, input: &[f32]) -> Vec<i16> {
        let Some(inner) = self.inner.as_mut() else {
            return input.iter().copied().map(to_i16).collect();
        };

        self.pending.extend_from_slice(input);
        let mut out = Vec::new();

        while self.pending.len() >= self.chunk_in {
            let chan = &mut self.scratch_in[0];
            chan.clear();
            chan.extend_from_slice(&self.pending[..self.chunk_in]);
            self.pending.drain(..self.chunk_in);

            // Drop the chunk rather than the stream; tearing down loses a
            // buffer the caller may be minutes into.
            match inner.process(&self.scratch_in, None) {
                Ok(frames) => match frames.first() {
                    Some(mono) => out.extend(mono.iter().copied().map(to_i16)),
                    None => self.dropped_chunks += 1,
                },
                Err(_) => self.dropped_chunks += 1,
            }

            self.chunk_in = inner.input_frames_next();
        }

        out
    }

    /// Chunks refused since construction. Nonzero means the output is shorter
    /// than the input accounts for.
    pub fn dropped_chunks(&self) -> u64 {
        self.dropped_chunks
    }
}

/// Clamp first: resampling can overshoot past 1.0 even when the input did not,
/// and wrapping would be audible as a click.
#[inline]
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_rates_pass_through() {
        let mut r = Resampler::new(16_000, 16_000).unwrap();
        let out = r.push(&[0.0, 0.5, -0.5, 1.0]);
        assert_eq!(out, vec![0, 16383, -16383, 32767]);
    }

    #[test]
    fn downsamples_by_rate_ratio() {
        let mut r = Resampler::new(48_000, 16_000).unwrap();

        // 1s at 48 kHz yields ~1s at 16 kHz, less the final partial chunk.
        let input: Vec<f32> = (0..48_000)
            .map(|i| ((i as f32 / 48_000.0) * std::f32::consts::TAU * 440.0).sin() * 0.5)
            .collect();

        let out = r.push(&input);
        let ratio = out.len() as f32 / 16_000.0;
        assert!(ratio > 0.9 && ratio <= 1.0, "got {} samples", out.len());
    }

    #[test]
    fn small_writes_accumulate() {
        let mut r = Resampler::new(48_000, 16_000).unwrap();
        let mut total = 0;
        for _ in 0..1000 {
            total += r.push(&[0.1; 128]).len();
        }
        assert!(total > 0, "128-sample writes produced nothing");
    }

    #[test]
    fn clamps_out_of_range() {
        let mut r = Resampler::new(16_000, 16_000).unwrap();
        assert_eq!(r.push(&[9.0, -9.0]), vec![32767, -32767]);
    }

    #[test]
    fn rejects_zero_rate() {
        assert!(Resampler::new(0, 16_000).is_err());
        assert!(Resampler::new(48_000, 0).is_err());
    }
}
