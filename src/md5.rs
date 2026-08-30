//! MD5 message digest (RFC 1321) — the hash the new-era descriptor's
//! `cFileMD5` field carries (`format-reference.md` §1.1), computed over
//! the §1.8 block concatenation by the encoder-side container writer
//! ([`crate::writer`]).
//!
//! A self-contained streaming implementation of the public algorithm:
//! four rounds of sixteen steps over 64-byte blocks, the standard
//! per-step rotation schedule and additive constants, little-endian
//! length padding. Verified against the RFC test vectors and — the
//! check that matters here — against the digest every vendor-encoded
//! fixture stores.

/// Per-step left-rotation amounts, four per round.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Per-step additive constants (`floor(|sin(i + 1)| * 2^32)`).
const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// Streaming MD5 state.
#[derive(Debug, Clone)]
pub struct Md5 {
    state: [u32; 4],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// Fresh digest state.
    pub const fn new() -> Self {
        Md5 {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            block: [0; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    /// One-shot digest of `data`.
    pub fn digest(data: &[u8]) -> [u8; 16] {
        let mut h = Self::new();
        h.update(data);
        h.finish()
    }

    /// Absorb `data`.
    pub fn update(&mut self, data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        let mut rest = data;
        if self.block_len > 0 {
            let take = (64 - self.block_len).min(rest.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&rest[..take]);
            self.block_len += take;
            rest = &rest[take..];
            if self.block_len < 64 {
                return; // the whole input fit in the partial block
            }
            let block = self.block;
            self.compress(&block);
            self.block_len = 0;
        }
        let mut chunks = rest.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }
        let tail = chunks.remainder();
        self.block[..tail.len()].copy_from_slice(tail);
        self.block_len = tail.len();
    }

    /// Pad, absorb the bit length, and return the 16-byte digest.
    pub fn finish(mut self) -> [u8; 16] {
        let bit_len = self.total_len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        // Pad to 56 mod 64 (counting the 0x80 already pushed).
        let target = 56usize;
        let have = (self.block_len + 1) % 64;
        let zeros = if have <= target {
            target - have
        } else {
            64 + target - have
        };
        pad.resize(1 + zeros, 0u8);
        pad.extend_from_slice(&bit_len.to_le_bytes());
        // `update` would count the padding into `total_len`; feed it
        // through the block machinery directly.
        let saved = self.total_len;
        self.update(&pad);
        self.total_len = saved;
        debug_assert_eq!(self.block_len, 0);
        let mut out = [0u8; 16];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// One 64-byte block through the four rounds.
    fn compress(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g])
                .rotate_left(S[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: &[u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc_1321_test_vectors() {
        assert_eq!(hex(&Md5::digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&Md5::digest(b"a")), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(
            hex(&Md5::digest(b"abc")),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex(&Md5::digest(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(&Md5::digest(b"abcdefghijklmnopqrstuvwxyz")),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            hex(&Md5::digest(
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
            )),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
        assert_eq!(
            hex(&Md5::digest(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn streaming_matches_one_shot_at_every_split() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 + 3) as u8).collect();
        let whole = Md5::digest(&data);
        for split in 0..data.len() {
            let mut h = Md5::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), whole, "split {split}");
        }
        // Byte-at-a-time.
        let mut h = Md5::new();
        for b in &data {
            h.update(core::slice::from_ref(b));
        }
        assert_eq!(h.finish(), whole);
    }

    #[test]
    fn padding_boundaries_55_56_63_64_bytes() {
        // Lengths around the 56-mod-64 padding boundary.
        for len in [55usize, 56, 57, 63, 64, 65, 119, 120, 128] {
            let data = vec![0x61u8; len];
            let mut h = Md5::new();
            h.update(&data);
            assert_eq!(h.finish(), Md5::digest(&data), "len {len}");
        }
        assert_eq!(
            hex(&Md5::digest(&[0x61u8; 64])),
            "014842d480b571495a4a0363793f7367"
        );
    }
}
