//! Time-addressed retention ring for mono `i16` samples at the target rate.
//!
//! Allocated once at a ceiling and never grown. `retention` is an adjustable
//! window inside that ceiling, so resizing history costs an assignment.

/// Mono i16 at 16 kHz is ~1.92 MB/min; 60 min is ~115 MB of address space,
/// touched only up to the active retention.
pub const DEFAULT_CEILING_SECS: u32 = 60 * 60;

pub struct RetentionRing {
    buf: Vec<i16>,
    /// Frames the ring can physically hold. Fixed for the ring's life.
    capacity: usize,
    /// Frames to expose. `<= capacity`. Adjustable at runtime.
    retention: usize,
    /// Next write index.
    write: usize,
    /// Frames physically written, saturating at `capacity`.
    filled: usize,
    rate: u32,
}

impl RetentionRing {
    pub fn new(rate: u32, ceiling_frames: usize, retention_frames: usize) -> Self {
        let capacity = ceiling_frames.max(1);
        Self {
            buf: vec![0; capacity],
            capacity,
            retention: retention_frames.clamp(1, capacity),
            write: 0,
            filled: 0,
            rate,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn set_retention_frames(&mut self, frames: usize) {
        self.retention = frames.clamp(1, self.capacity);
    }

    pub fn retention_frames(&self) -> usize {
        self.retention
    }

    /// Frames visible: everything written, capped by retention. Lowering
    /// retention hides older frames without discarding them.
    pub fn window_frames(&self) -> usize {
        self.filled.min(self.retention)
    }

    pub fn window_ms(&self) -> u32 {
        ((self.window_frames() as u64 * 1000) / self.rate.max(1) as u64) as u32
    }

    pub fn push(&mut self, samples: &[i16]) {
        // A push larger than the ring can only leave its own tail.
        let samples = if samples.len() > self.capacity {
            &samples[samples.len() - self.capacity..]
        } else {
            samples
        };

        let first = (self.capacity - self.write).min(samples.len());
        self.buf[self.write..self.write + first].copy_from_slice(&samples[..first]);
        let rest = samples.len() - first;
        if rest > 0 {
            self.buf[..rest].copy_from_slice(&samples[first..]);
        }

        self.write = (self.write + samples.len()) % self.capacity;
        self.filled = (self.filled + samples.len()).min(self.capacity);
    }

    /// Index of the oldest visible frame.
    fn window_start(&self) -> usize {
        (self.write + self.capacity - self.window_frames()) % self.capacity
    }

    /// Copy a frame range out. Offsets are relative to the oldest visible frame.
    pub fn read_frames(&self, start: usize, end: usize) -> Vec<i16> {
        let window = self.window_frames();
        let start = start.min(window);
        let end = end.min(window);
        if end <= start {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(end - start);
        let base = self.window_start();
        for i in start..end {
            out.push(self.buf[(base + i) % self.capacity]);
        }
        out
    }

    pub fn ms_to_frames(&self, ms: u32) -> usize {
        ((ms as u64 * self.rate as u64) / 1000) as usize
    }

    /// Min/max per bucket over the visible window, interleaved as
    /// `[min0, max0, min1, max1, ...]`, normalised to -1.0..=1.0.
    pub fn peaks(&self, buckets: usize) -> Vec<f32> {
        let buckets = buckets.max(1);
        let window = self.window_frames();
        let mut out = vec![0.0f32; buckets * 2];
        if window == 0 {
            return out;
        }

        let base = self.window_start();
        for b in 0..buckets {
            let from = (b * window) / buckets;
            let to = (((b + 1) * window) / buckets).max(from + 1).min(window);

            let mut lo = i16::MAX;
            let mut hi = i16::MIN;
            for i in from..to {
                let s = self.buf[(base + i) % self.capacity];
                if s < lo {
                    lo = s;
                }
                if s > hi {
                    hi = s;
                }
            }

            // i16::MIN negates to 32768 and overflows i16; scaling by 32768.0
            // maps both bounds into range without a special case.
            out[b * 2] = lo as f32 / 32768.0;
            out[b * 2 + 1] = hi as f32 / 32768.0;
        }
        out
    }

    pub fn clear(&mut self) {
        self.write = 0;
        self.filled = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> RetentionRing {
        RetentionRing::new(1000, 10, 10)
    }

    #[test]
    fn reads_back_pushed_samples() {
        let mut r = ring();
        r.push(&[1, 2, 3]);
        assert_eq!(r.window_frames(), 3);
        assert_eq!(r.read_frames(0, 3), vec![1, 2, 3]);
    }

    #[test]
    fn wraps_and_keeps_tail() {
        let mut r = ring();
        r.push(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(r.window_frames(), 10);
        assert_eq!(r.read_frames(0, 10), vec![3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn oversized_push_keeps_its_tail() {
        let mut r = ring();
        let big: Vec<i16> = (1..=25).collect();
        r.push(&big);
        assert_eq!(
            r.read_frames(0, 10),
            vec![16, 17, 18, 19, 20, 21, 22, 23, 24, 25]
        );
    }

    #[test]
    fn retention_hides_and_restores() {
        let mut r = ring();
        r.push(&[1, 2, 3, 4, 5, 6, 7, 8]);

        r.set_retention_frames(3);
        assert_eq!(r.read_frames(0, 3), vec![6, 7, 8]);

        r.set_retention_frames(8);
        assert_eq!(r.read_frames(0, 8), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn ranges_clamp() {
        let mut r = ring();
        r.push(&[1, 2, 3]);
        assert_eq!(r.read_frames(2, 99), vec![3]);
        assert!(r.read_frames(5, 2).is_empty());
        assert!(r.read_frames(0, 0).is_empty());
    }

    #[test]
    fn peaks_bracket_signal() {
        let mut r = ring();
        r.push(&[i16::MIN, i16::MAX]);
        let p = r.peaks(1);
        assert_eq!(p.len(), 2);
        assert!((p[0] - -1.0).abs() < 1e-6);
        assert!(p[1] > 0.99 && p[1] <= 1.0);
    }

    #[test]
    fn empty_ring_peaks_are_flat() {
        assert_eq!(ring().peaks(4), vec![0.0; 8]);
    }
}
