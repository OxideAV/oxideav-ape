//! Carryless byte-oriented range coder — the entropy-layer core the
//! staged `docs/audio/ape/format-reference.md` §2 pins.
//!
//! The staged reference commits to:
//!
//! * the coder constants (§2.1) — `CODE_BITS = 32`, `TOP_VALUE = 1 <<
//!   31`, `SHIFT_BITS = CODE_BITS - 9`, `EXTRA_BITS = (CODE_BITS - 2) %
//!   8 + 1`, `BOTTOM_VALUE = TOP_VALUE >> 8`;
//! * the byte-input convention (§2.2) — the compressed frame is a
//!   big-endian bit array addressed as 32-bit words, the byte at bit
//!   index `i` being `word[i >> 5] >> (24 - (i & 31))` masked to 8
//!   bits, each byte read advancing the index by 8;
//! * the renormalisation sequence (§2.3) — while `range <=
//!   BOTTOM_VALUE`: roll the input byte into `buffer`, absorb
//!   `(buffer >> 1) & 0xFF` into `low`, scale `range` by 256. The
//!   `(buffer >> 1) & 0xFF` is the carryless detail: `buffer` is a
//!   9-bit rolling window whose least-significant bit is a pending
//!   carry, so `low` absorbs bits 1..8 of that window, not the raw
//!   byte;
//! * the two decode primitives (§2.4) — *decode-culfreq* (`range >>=
//!   shift`; return `low / range`, **without** consuming) and
//!   *decode-and-consume* (`range >>= shift`; `v = low / range`; `low
//!   -= range * v`), plus the interval-narrowing step for a looked-up
//!   symbol (`low -= range * cumfreq; range *= width`).
//!
//! ## Register priming (documented inference)
//!
//! The exact register prime sequence at frame start is a **GAP** in the
//! staged reference (§4: "renormalise on demand" only). This module
//! primes with the one construction the staged constants themselves
//! determine: the first input byte seeds `buffer`, `low` takes that
//! byte's top `EXTRA_BITS` bits (`byte >> (8 - EXTRA_BITS)`), and
//! `range` starts at `1 << EXTRA_BITS`. `EXTRA_BITS = 7` has no other
//! role in the staged constant set, and the §2.3 renormalisation then
//! absorbs a *contiguous* bit prefix of the stream (7 bits, then 8 per
//! refill), which is the only alignment under which `low` tracks the
//! byte expansion of the coded value. Real-file validation against a
//! vendor-encoded fixture exercises this inference end-to-end; if a
//! future trace pins a different prime, only [`RangeDecoder::new`]
//! moves.
//!
//! The encoder half ([`RangeEncoder`]) is **not** described by the
//! staged reference; it is derived in this crate as the arithmetic
//! mirror of the pinned decoder (accumulate `v * range` where the
//! decoder recovers `v = low / range`, emit the 31-bit `low` window's
//! top byte per renormalisation, back-propagate carries through
//! emitted bytes). Its correctness criterion is exactly the round-trip
//! property the tests pin: whatever the encoder writes, the pinned
//! decoder sequence reads back.
//!
//! All interval arithmetic uses wrapping operations: a corrupt stream
//! may break the `low < range` invariant a well-formed stream
//! maintains, and the coder must stay panic-free and merely produce
//! garbage values (higher layers detect corruption); the only hard
//! stops are division-by-zero guards surfaced as
//! [`Error::CorruptStream`].

use crate::error::{Error, Result};

/// Coder register width in bits (§2.1).
pub const CODE_BITS: u32 = 32;

/// High mark of the coder interval: `1 << 31` (§2.1).
pub const TOP_VALUE: u32 = 1 << (CODE_BITS - 1);

/// Byte-emission shift: `CODE_BITS - 9` = 23 (§2.1). The encoder-side
/// mirror emits `low >> SHIFT_BITS` — the top 8 bits of the 31-bit
/// `low` window — each renormalisation.
pub const SHIFT_BITS: u32 = CODE_BITS - 9;

/// Initial-fill width: `(CODE_BITS - 2) % 8 + 1` = 7 (§2.1). The
/// number of payload bits the first input byte contributes when the
/// decoder registers are primed.
pub const EXTRA_BITS: u32 = (CODE_BITS - 2) % 8 + 1;

/// Renormalisation threshold: `TOP_VALUE >> 8` = `0x0080_0000` (§2.1).
/// Both directions renormalise while `range <= BOTTOM_VALUE`.
pub const BOTTOM_VALUE: u32 = TOP_VALUE >> 8;

/// §2.2 byte input: a big-endian bit array addressed as 32-bit words.
///
/// The byte at bit index `i` is `word[i >> 5] >> (24 - (i & 31))`
/// masked to 8 bits — bytes are consumed MSB-first within each 32-bit
/// word — and every read advances the bit index by 8. For byte-aligned
/// starts this walks the buffer sequentially; the general word-window
/// form below also serves non-aligned bit indices (the `<= 3800`
/// seek-bit-table era can start a frame mid-byte).
///
/// Reads past the end of the buffer return zero bytes and are counted
/// in [`BitInput::overrun_bytes`] — the range decoder legitimately
/// reads a few bytes of lookahead past the final symbol, and the
/// zero-fill keeps that read in-bounds without the caller having to
/// over-allocate.
#[derive(Debug, Clone)]
pub struct BitInput<'a> {
    data: &'a [u8],
    bit_pos: u64,
    overrun: u32,
    le_words: bool,
}

impl<'a> BitInput<'a> {
    /// A bit array over `data`, positioned at `start_bit`, with the
    /// 32-bit words assembled big-endian from the bytes (word value ==
    /// sequential byte order).
    pub fn new(data: &'a [u8], start_bit: u64) -> Self {
        BitInput {
            data,
            bit_pos: start_bit,
            overrun: 0,
            le_words: false,
        }
    }

    /// A bit array over `data` whose 32-bit words are **loaded
    /// little-endian** from the bytes and then consumed MSB-first —
    /// i.e. the byte consumption order is reversed within each 4-byte
    /// group. This is the layout real vendor-encoded frames use
    /// (established black-box against vendor-encoded fixtures; the
    /// staged reference's "bit array addressed as 32-bit words" phrase
    /// describes exactly this word-load indirection).
    pub fn new_le_words(data: &'a [u8], start_bit: u64) -> Self {
        BitInput {
            data,
            bit_pos: start_bit,
            overrun: 0,
            le_words: true,
        }
    }

    /// The 32-bit word at word index `index`, zero-padded past the end
    /// of the buffer, assembled per the constructor's byte order.
    fn word(&self, index: u64) -> u32 {
        let base = index.saturating_mul(4);
        let mut bytes = [0u8; 4];
        for (k, slot) in bytes.iter_mut().enumerate() {
            *slot = usize::try_from(base + k as u64)
                .ok()
                .and_then(|i| self.data.get(i).copied())
                .unwrap_or(0);
        }
        if self.le_words {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    }

    /// Read the byte at the current bit index per the §2.2 addressing
    /// (`word[i >> 5] >> (24 - (i & 31))`, generalised across a word
    /// boundary for non-aligned `i`), advancing the index by 8.
    pub fn read_byte(&mut self) -> u8 {
        let i = self.bit_pos;
        let off = (i & 31) as u32;
        let window = (u64::from(self.word(i >> 5)) << 32) | u64::from(self.word((i >> 5) + 1));
        let byte = ((window >> (56 - off)) & 0xFF) as u8;
        if (i / 8) >= self.data.len() as u64 {
            self.overrun = self.overrun.saturating_add(1);
        }
        self.bit_pos = i + 8;
        byte
    }

    /// Read 32 bits MSB-first (four byte reads) — the width the vendor
    /// frame prologue words are read at through the bit array.
    pub fn read_u32(&mut self) -> u32 {
        let mut w = 0u32;
        for _ in 0..4 {
            w = (w << 8) | u32::from(self.read_byte());
        }
        w
    }

    /// Current bit index.
    pub fn bit_pos(&self) -> u64 {
        self.bit_pos
    }

    /// Number of byte reads that started at or past the end of the
    /// buffer (zero-filled lookahead).
    pub fn overrun_bytes(&self) -> u32 {
        self.overrun
    }
}

/// §2.2–§2.4 range decoder: `low` / `range` registers plus the rolling
/// 9-bit `buffer` window over the byte input.
#[derive(Debug, Clone)]
pub struct RangeDecoder<'a> {
    input: BitInput<'a>,
    low: u32,
    range: u32,
    buffer: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Prime the registers over `data` starting at byte 0.
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_input(BitInput::new(data, 0))
    }

    /// Prime the registers over an explicit [`BitInput`] (for frame
    /// payloads that start at a non-zero bit offset).
    ///
    /// The prime sequence — first byte into `buffer`, its top
    /// [`EXTRA_BITS`] bits into `low`, `range = 1 << EXTRA_BITS` — is
    /// the constants-determined inference documented at module level
    /// (the exact sequence is a staged-reference GAP).
    pub fn with_input(mut input: BitInput<'a>) -> Self {
        let first = u32::from(input.read_byte());
        RangeDecoder {
            input,
            buffer: first,
            low: first >> (8 - EXTRA_BITS),
            range: 1 << EXTRA_BITS,
        }
    }

    /// §2.3 renormalisation: while `range <= BOTTOM_VALUE`, roll a byte
    /// into `buffer`, absorb `(buffer >> 1) & 0xFF` into `low`, scale
    /// `range` by 256.
    fn normalize(&mut self) {
        while self.range <= BOTTOM_VALUE {
            self.buffer = (self.buffer.wrapping_shl(8)) | u32::from(self.input.read_byte());
            self.low = (self.low.wrapping_shl(8)) | ((self.buffer >> 1) & 0xFF);
            self.range = self.range.wrapping_shl(8);
        }
    }

    /// §2.4 *decode-culfreq*: renormalise, `range >>= shift`, and
    /// return `low / range` **without** consuming it from the interval.
    /// The caller looks the value up against the cumulative-frequency
    /// table and then narrows via [`RangeDecoder::consume`].
    pub fn decode_culfreq(&mut self, shift: u32) -> Result<u32> {
        self.normalize();
        self.range >>= shift;
        if self.range == 0 {
            return Err(Error::CorruptStream("range underflow in decode_culfreq"));
        }
        Ok(self.low / self.range)
    }

    /// §2.4 interval narrowing for a looked-up symbol: `low -= range *
    /// cumfreq; range *= width`. Must follow a
    /// [`RangeDecoder::decode_culfreq`] on the already-shifted `range`.
    pub fn consume(&mut self, cumfreq: u32, width: u32) {
        self.low = self.low.wrapping_sub(self.range.wrapping_mul(cumfreq));
        self.range = self.range.wrapping_mul(width);
    }

    /// §2.4 *decode-and-consume* over a power-of-two radix: renormalise,
    /// `range >>= shift`, read and consume a `shift`-bit value.
    pub fn decode_bits(&mut self, shift: u32) -> Result<u32> {
        self.normalize();
        self.range >>= shift;
        if self.range == 0 {
            return Err(Error::CorruptStream("range underflow in decode_bits"));
        }
        let v = self.low / self.range;
        self.low = self.low.wrapping_sub(self.range.wrapping_mul(v));
        Ok(v)
    }

    /// §2.6 arbitrary-radix *decode-and-consume*: renormalise, `range /=
    /// divisor`, read and consume a value in `[0, divisor)` (division-
    /// based, not whole bits — the ≥ 3990 base decode).
    pub fn decode_base(&mut self, divisor: u32) -> Result<u32> {
        if divisor == 0 {
            return Err(Error::CorruptStream("zero divisor in decode_base"));
        }
        self.normalize();
        self.range /= divisor;
        if self.range == 0 {
            return Err(Error::CorruptStream("range underflow in decode_base"));
        }
        let v = self.low / self.range;
        self.low = self.low.wrapping_sub(self.range.wrapping_mul(v));
        Ok(v)
    }

    /// Bit index of the underlying input (diagnostics / frame-boundary
    /// accounting).
    pub fn bit_pos(&self) -> u64 {
        self.input.bit_pos()
    }

    /// Zero-filled lookahead byte reads past the end of the input.
    pub fn overrun_bytes(&self) -> u32 {
        self.input.overrun_bytes()
    }
}

/// Encoder-direction mirror of [`RangeDecoder`] (crate-derived, not
/// staged — see the module docs). Accumulates `v * range` where the
/// decoder recovers `v = low / range`, emitting the top byte of the
/// 31-bit `low` window per renormalisation with carry back-propagation
/// through the emitted bytes.
#[derive(Debug, Clone, Default)]
pub struct RangeEncoder {
    low: u32,
    range: u32,
    out: Vec<u8>,
}

impl RangeEncoder {
    /// Fresh encoder: `low = 0`, `range = TOP_VALUE` — the state the
    /// decoder reaches after priming plus its first renormalisation.
    pub fn new() -> Self {
        RangeEncoder {
            low: 0,
            range: TOP_VALUE,
            out: Vec::new(),
        }
    }

    /// Mirror of the decoder's §2.3 renormalisation: emit the top byte
    /// of the `low` window while `range <= BOTTOM_VALUE`.
    fn normalize(&mut self) {
        while self.range <= BOTTOM_VALUE {
            self.shift_low();
            self.range = self.range.wrapping_shl(8);
        }
    }

    fn shift_low(&mut self) {
        self.out.push((self.low >> SHIFT_BITS) as u8);
        self.low = (self.low & (BOTTOM_VALUE - 1)) << 8;
    }

    /// Add `add` into the `low` accumulator, back-propagating a carry
    /// out of the 31-bit window into the already-emitted bytes.
    fn add_low(&mut self, add: u32) {
        let sum = self.low.wrapping_add(add);
        if sum >= TOP_VALUE {
            // Carry: +1 to the emitted byte string, walking back over
            // any 0xFF run. A carry escaping past the first emitted
            // byte would mean the accumulated value exceeded TOP_VALUE,
            // which the interval algebra rules out.
            for b in self.out.iter_mut().rev() {
                if *b == 0xFF {
                    *b = 0;
                } else {
                    *b += 1;
                    break;
                }
            }
        }
        self.low = sum & (TOP_VALUE - 1);
    }

    /// Mirror of [`RangeDecoder::decode_bits`]: encode `v` in
    /// `[0, 1 << shift)`.
    pub fn encode_bits(&mut self, v: u32, shift: u32) {
        self.normalize();
        self.range >>= shift;
        debug_assert!(u64::from(v) < (1u64 << shift) || shift == 0);
        self.add_low(v.wrapping_mul(self.range));
    }

    /// Mirror of [`RangeDecoder::decode_base`]: encode `v` in
    /// `[0, divisor)` (arbitrary radix, division-based).
    pub fn encode_base(&mut self, v: u32, divisor: u32) {
        debug_assert!(divisor > 0 && v < divisor);
        self.normalize();
        self.range /= divisor;
        self.add_low(v.wrapping_mul(self.range));
    }

    /// Mirror of [`RangeDecoder::decode_culfreq`] +
    /// [`RangeDecoder::consume`]: encode a symbol occupying the
    /// cumulative-frequency interval `[cumfreq, cumfreq + width)` of a
    /// model whose total is `1 << shift`.
    pub fn encode_interval(&mut self, cumfreq: u32, width: u32, shift: u32) {
        self.normalize();
        self.range >>= shift;
        self.add_low(cumfreq.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(width);
    }

    /// Flush the remaining `low` window and return the byte stream.
    /// Emits four bytes — the full 31-bit window — so the decoder's
    /// eager lookahead always lands on determined bytes.
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..4 {
            self.shift_low();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_staged_reference() {
        assert_eq!(CODE_BITS, 32);
        assert_eq!(TOP_VALUE, 0x8000_0000);
        assert_eq!(SHIFT_BITS, 23);
        assert_eq!(EXTRA_BITS, 7);
        assert_eq!(BOTTOM_VALUE, 0x0080_0000);
        assert_eq!(BOTTOM_VALUE, TOP_VALUE >> 8);
    }

    #[test]
    fn bit_input_walks_bytes_sequentially_when_aligned() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
        let mut input = BitInput::new(&data, 0);
        for &b in &data {
            assert_eq!(input.read_byte(), b);
        }
        assert_eq!(input.overrun_bytes(), 0);
        // Past the end: zero-filled, counted.
        assert_eq!(input.read_byte(), 0);
        assert_eq!(input.read_byte(), 0);
        assert_eq!(input.overrun_bytes(), 2);
    }

    #[test]
    fn bit_input_matches_the_staged_word_addressing() {
        // §2.2: byte at bit index i is word[i >> 5] >> (24 - (i & 31)).
        let data = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        for byte_index in 0..8u64 {
            let i = byte_index * 8;
            let word_index = (i >> 5) as usize;
            let word = u32::from_be_bytes([
                data[word_index * 4],
                data[word_index * 4 + 1],
                data[word_index * 4 + 2],
                data[word_index * 4 + 3],
            ]);
            let expected = ((word >> (24 - (i & 31))) & 0xFF) as u8;
            let mut input = BitInput::new(&data, i);
            assert_eq!(input.read_byte(), expected, "bit index {i}");
            // And it equals the plain sequential byte.
            assert_eq!(expected, data[byte_index as usize]);
        }
    }

    #[test]
    fn bit_input_supports_non_aligned_bit_positions() {
        // Bits 4..12 of 0xAB 0xCD = 0xBC.
        let data = [0xAB, 0xCD];
        let mut input = BitInput::new(&data, 4);
        assert_eq!(input.read_byte(), 0xBC);
        // Straddling the 32-bit word boundary: bits 28..36 of
        // 0x11223344 0x55... = low nibble of 0x44 ++ high nibble of 0x55.
        let data2 = [0x11, 0x22, 0x33, 0x44, 0x55];
        let mut input2 = BitInput::new(&data2, 28);
        assert_eq!(input2.read_byte(), 0x45);
    }

    #[test]
    fn decoder_priming_takes_extra_bits_of_the_first_byte() {
        let data = [0xAB, 0, 0, 0, 0];
        let dec = RangeDecoder::new(&data);
        assert_eq!(dec.low, 0xAB >> 1);
        assert_eq!(dec.range, 1 << EXTRA_BITS);
        assert_eq!(dec.buffer, 0xAB);
    }

    #[test]
    fn single_bits_value_round_trips() {
        for shift in [0u32, 1, 5, 8, 15, 16] {
            for v in [0u32, 1, (1 << shift) - 1, (1 << shift) / 2] {
                let v = v & ((1u32 << shift) - 1);
                let mut enc = RangeEncoder::new();
                enc.encode_bits(v, shift);
                let bytes = enc.finish();
                let mut dec = RangeDecoder::new(&bytes);
                assert_eq!(dec.decode_bits(shift).unwrap(), v, "shift {shift} v {v}");
            }
        }
    }

    #[test]
    fn zero_shift_decodes_zero_without_consuming_interval() {
        // decode_bits(0) must return 0 on a well-formed stream (the
        // nTempK = 0 path of the old coder decodes zero low bits).
        let mut enc = RangeEncoder::new();
        enc.encode_bits(0x55, 8);
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        assert_eq!(dec.decode_bits(0).unwrap(), 0);
        assert_eq!(dec.decode_bits(8).unwrap(), 0x55);
    }

    #[test]
    fn bits_sequence_round_trips() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut xs = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let seq: Vec<(u32, u32)> = (0..4096)
            .map(|_| {
                let shift = (xs() % 17) as u32; // 0..=16
                let v = if shift == 0 {
                    0
                } else {
                    (xs() as u32) & ((1u32 << shift) - 1)
                };
                (v, shift)
            })
            .collect();
        let mut enc = RangeEncoder::new();
        for &(v, shift) in &seq {
            enc.encode_bits(v, shift);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        for &(v, shift) in &seq {
            assert_eq!(dec.decode_bits(shift).unwrap(), v);
        }
        assert_eq!(dec.overrun_bytes(), 0, "flush must cover the lookahead");
    }

    #[test]
    fn base_sequence_round_trips_over_arbitrary_radices() {
        let mut state = 0xC0FF_EE11_2233_4455u64;
        let mut xs = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let seq: Vec<(u32, u32)> = (0..4096)
            .map(|_| {
                let divisor = ((xs() % 65535) + 1) as u32; // 1..=65535
                let v = (xs() as u32) % divisor;
                (v, divisor)
            })
            .collect();
        let mut enc = RangeEncoder::new();
        for &(v, d) in &seq {
            enc.encode_base(v, d);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        for &(v, d) in &seq {
            assert_eq!(dec.decode_base(d).unwrap(), v);
        }
    }

    #[test]
    fn interval_symbols_round_trip_against_both_staged_models() {
        use crate::freq_model::{
            symbol_for_cum_freq, symbol_interval, COUNTS_GE3990, COUNTS_LE3980, MODEL_ELEMENTS,
            RANGE_OVERFLOW_SHIFT,
        };
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            let symbols: Vec<usize> = (0..2048)
                .map(|i| (i * 7 + i / 5) % MODEL_ELEMENTS)
                .collect();
            let mut enc = RangeEncoder::new();
            for &s in &symbols {
                let (low, width) = symbol_interval(counts, s).unwrap();
                enc.encode_interval(low, width, RANGE_OVERFLOW_SHIFT);
            }
            let bytes = enc.finish();
            let mut dec = RangeDecoder::new(&bytes);
            for &s in &symbols {
                let cf = dec.decode_culfreq(RANGE_OVERFLOW_SHIFT).unwrap();
                let sym = symbol_for_cum_freq(counts, cf);
                assert_eq!(sym, s);
                let (low, width) = symbol_interval(counts, sym).unwrap();
                dec.consume(low, width);
            }
        }
    }

    #[test]
    fn mixed_primitive_stream_round_trips() {
        // Interleave interval symbols, bit fields, and arbitrary-radix
        // bases the way the residual layer does.
        use crate::freq_model::{
            symbol_for_cum_freq, symbol_interval, COUNTS_GE3990, RANGE_OVERFLOW_SHIFT,
        };
        let counts = &COUNTS_GE3990;
        let mut enc = RangeEncoder::new();
        let script: Vec<(usize, u32, u32, u32)> = (0..1024)
            .map(|i| {
                let sym = (i * 11) % 64;
                let bits_v = (i as u32).wrapping_mul(2654435761) & 0xFFFF;
                let divisor = (i as u32 % 1000) + 1;
                let base_v = (i as u32 * 40503) % divisor;
                (sym, bits_v, divisor, base_v)
            })
            .collect();
        for &(sym, bits_v, divisor, base_v) in &script {
            let (low, width) = symbol_interval(counts, sym).unwrap();
            enc.encode_interval(low, width, RANGE_OVERFLOW_SHIFT);
            enc.encode_bits(bits_v, 16);
            enc.encode_base(base_v, divisor);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        for &(sym, bits_v, divisor, base_v) in &script {
            let cf = dec.decode_culfreq(RANGE_OVERFLOW_SHIFT).unwrap();
            let got = symbol_for_cum_freq(counts, cf);
            assert_eq!(got, sym);
            let (low, width) = symbol_interval(counts, got).unwrap();
            dec.consume(low, width);
            assert_eq!(dec.decode_bits(16).unwrap(), bits_v);
            assert_eq!(dec.decode_base(divisor).unwrap(), base_v);
        }
    }

    #[test]
    fn carry_propagation_survives_ff_runs() {
        // Values engineered so the accumulated low crosses byte
        // boundaries repeatedly; the round-trip is the proof the carry
        // walk-back is correct.
        let mut enc = RangeEncoder::new();
        let seq: Vec<u32> = (0..512)
            .map(|i| if i % 3 == 0 { 0xFFFF } else { 1 })
            .collect();
        for &v in &seq {
            enc.encode_bits(v, 16);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        for &v in &seq {
            assert_eq!(dec.decode_bits(16).unwrap(), v);
        }
    }

    #[test]
    fn decoder_is_panic_free_on_arbitrary_bytes() {
        // Corrupt / random streams must produce values (possibly
        // garbage) or CorruptStream errors, never a panic.
        let mut state = 0x0BAD_F00D_DEAD_BEEFu64;
        for _ in 0..64 {
            let bytes: Vec<u8> = (0..96)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            let mut dec = RangeDecoder::new(&bytes);
            for i in 0..256u32 {
                let _ = dec.decode_culfreq(16);
                dec.consume(i % 65536, (i % 512) + 1);
                let _ = dec.decode_bits(i % 17);
                let _ = dec.decode_base((i % 4096) + 1);
            }
        }
    }

    #[test]
    fn empty_input_decodes_zeroes() {
        // Zero-filled input: every primitive reads zero values.
        let mut dec = RangeDecoder::new(&[]);
        assert_eq!(dec.decode_bits(16).unwrap(), 0);
        assert_eq!(dec.decode_base(12345).unwrap(), 0);
        assert!(dec.overrun_bytes() > 0);
    }

    #[test]
    fn encoder_finish_emits_the_full_window() {
        // A fresh encoder flushes exactly the four window bytes.
        let enc = RangeEncoder::new();
        assert_eq!(enc.finish(), vec![0, 0, 0, 0]);
    }
}
