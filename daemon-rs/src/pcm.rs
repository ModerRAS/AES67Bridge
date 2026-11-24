//! Sample format decoding. Output is always 24-bit left aligned in an i32,
//! exactly like the C daemon (`v5.x` contract): S16 << 8, S24 packed sign
//! extended, S24_32 >> 8, float scaled by 8388607/8388608.

pub const MAX_CH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFmt {
    S16,
    S24Packed,
    S24_32,
    F32,
}

impl SampleFmt {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFmt::S16 => 2,
            SampleFmt::S24Packed => 3,
            SampleFmt::S24_32 | SampleFmt::F32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SampleFmt::S16 => "PCM_16",
            SampleFmt::S24Packed => "PCM_24_PACKED",
            SampleFmt::S24_32 => "PCM_24",
            SampleFmt::F32 => "FLOAT",
        }
    }

    /// Parse the numeric `fmt=0x...` HAL codes.
    pub fn from_hal_code(code: i64) -> Option<Self> {
        match code {
            1 => Some(SampleFmt::S16),
            3 | 4 => Some(SampleFmt::S24_32),
            5 => Some(SampleFmt::F32),
            6 => Some(SampleFmt::S24Packed),
            _ => None,
        }
    }

    /// Fallback for `fmt_name=PCM_16_BIT` / `FLOAT` style values (C order).
    pub fn from_name(text: &str) -> Self {
        let upper = text.to_ascii_uppercase();
        if upper.contains("PACKED") || upper.contains("24_PACKED") || upper.contains("S24P") {
            SampleFmt::S24Packed
        } else if upper.contains("PCM_24") || upper.contains("S24") {
            SampleFmt::S24_32
        } else if upper.contains("FLOAT") || upper.contains("F32") {
            SampleFmt::F32
        } else {
            SampleFmt::S16
        }
    }

    /// Decode one sample to 24-bit left-aligned i32.
    pub fn decode(self, bytes: &[u8]) -> i32 {
        match self {
            SampleFmt::S16 => i32::from(i16::from_le_bytes([bytes[0], bytes[1]])) << 8,
            SampleFmt::S24Packed => {
                let v = i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
                if v & 0x0080_0000 != 0 {
                    v | !0x00ff_ffff
                } else {
                    v
                }
            }
            SampleFmt::S24_32 => {
                let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                v >> 8
            }
            SampleFmt::F32 => {
                let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let d = f64::from(v) * 8_388_607.0;
                let d = d.clamp(-8_388_608.0, 8_388_607.0);
                (d + 0.5).floor() as i32
            }
        }
    }

    /// Decode one interleaved frame (channels * bps bytes) into 24-bit i32s.
    pub fn decode_frame(self, bytes: &[u8], channels: usize, frame: &mut [i32; MAX_CH]) {
        let bps = self.bytes_per_sample();
        for c in 0..channels {
            frame[c] = self.decode(&bytes[c * bps..c * bps + bps]);
        }
    }
}
