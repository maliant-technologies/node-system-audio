//! Minimal canonical WAV writer: mono, 16-bit PCM.
//!
//! A container rather than bare PCM so format sniffers accept it and the bytes
//! play in an `<audio>` element unchanged.

const HEADER_BYTES: u32 = 36;

pub fn mono_pcm16(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);

    // RIFF chunk. The magic is what format sniffers key on.
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(HEADER_BYTES + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    // fmt subchunk: PCM (1), 1 channel, 16 bits per sample.
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_riff_wave() {
        let w = mono_pcm16(&[0, 1, -1], 16_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
    }

    #[test]
    fn header_is_44_bytes() {
        let samples = [0i16; 100];
        let w = mono_pcm16(&samples, 16_000);
        assert_eq!(w.len(), 44 + 200);

        let riff_size = u32::from_le_bytes(w[4..8].try_into().unwrap());
        assert_eq!(riff_size as usize, w.len() - 8);

        let data_size = u32::from_le_bytes(w[40..44].try_into().unwrap());
        assert_eq!(data_size, 200);
    }

    #[test]
    fn declares_mono_16bit() {
        let w = mono_pcm16(&[0], 16_000);
        assert_eq!(u16::from_le_bytes(w[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(w[22..24].try_into().unwrap()), 1); // channels
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[28..32].try_into().unwrap()), 32_000); // byte rate
        assert_eq!(u16::from_le_bytes(w[32..34].try_into().unwrap()), 2); // block align
        assert_eq!(u16::from_le_bytes(w[34..36].try_into().unwrap()), 16); // bit depth
    }

    #[test]
    fn samples_are_little_endian() {
        let w = mono_pcm16(&[0x0102], 16_000);
        assert_eq!(&w[44..46], &[0x02, 0x01]);
    }

    #[test]
    fn empty_clip_is_valid() {
        let w = mono_pcm16(&[], 16_000);
        assert_eq!(w.len(), 44);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 0);
    }
}
