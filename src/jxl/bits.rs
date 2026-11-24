//! The bit writer and the number forms of JPEG XL headers.

/// Bits written least significant first, as JPEG XL reads them.
#[derive(Default)]
pub(super) struct Bits {
    pub(super) out: Vec<u8>,
    pub(super) acc: u64,
    pub(super) n: u32,
}

impl Bits {
    pub(super) fn put(&mut self, nbits: u32, v: u64) {
        debug_assert!(nbits <= 32 && (nbits == 32 || v < 1u64 << nbits));
        self.acc |= v << self.n;
        self.n += nbits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    pub(super) fn bit(&mut self, b: bool) {
        self.put(1, b as u64);
    }

    pub(super) fn len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    pub(super) fn pad(&mut self) {
        if self.n > 0 {
            self.put(8 - self.n, 0);
        }
    }

    pub(super) fn append(&mut self, o: &Bits) {
        if self.n == 0 {
            self.out.extend_from_slice(&o.out);
        } else {
            for &b in &o.out {
                self.put(8, b as u64);
            }
        }
        self.put(o.n, o.acc);
    }

    /// A U32 field: a 2-bit selector, then the bits of the chosen
    /// distribution, each `(offset, bits)`.
    pub(super) fn u32(&mut self, v: u32, dist: [(u32, u32); 4]) {
        for (sel, &(off, bits)) in dist.iter().enumerate() {
            if v >= off && ((v - off) as u64) < 1u64 << bits {
                self.put(2, sel as u64);
                self.put(bits, (v - off) as u64);
                return;
            }
        }
        unreachable!("value {v} does not fit its field");
    }

    pub(super) fn u64(&mut self, v: u64) {
        match v {
            0 => self.put(2, 0),
            1..=16 => {
                self.put(2, 1);
                self.put(4, v - 1);
            }
            17..=272 => {
                self.put(2, 2);
                self.put(8, v - 17);
            }
            _ => {
                self.put(2, 3);
                self.put(12, v & 0xFFF);
                let mut rest = v >> 12;
                while rest > 0 {
                    // Values here stay far below 2^60, the last group.
                    self.bit(true);
                    self.put(8, rest & 0xFF);
                    rest >>= 8;
                }
                self.bit(false);
            }
        }
    }

    pub(super) fn f16(&mut self, v: f32) {
        self.put(16, f16_bits(v) as u64);
    }
}

/// Binary16 bits of `v`, truncating the mantissa (the decoder sees exactly
/// these values, so the choice only needs to be consistent).
fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = (b >> 31) as u16;
    let exp = ((b >> 23) & 0xFF) as i32 - 127;
    let mant = b & 0x7F_FFFF;
    if exp < -24 {
        return sign << 15;
    }
    let (e16, m16) = if exp < -14 {
        let sub = (-14 - exp) as u32;
        (0, (1 << (10 - sub)) + (mant >> (13 + sub)))
    } else {
        debug_assert!(exp <= 15);
        ((exp + 15) as u32, mant >> 13)
    };
    (sign << 15) | ((e16 as u16) << 10) | m16 as u16
}

pub(super) fn ceil_log2(x: usize) -> u32 {
    if x <= 1 {
        0
    } else {
        usize::BITS - (x - 1).leading_zeros()
    }
}

pub(super) fn pack_signed(v: i32) -> u32 {
    if v >= 0 {
        (v as u32) << 1
    } else {
        ((-(v as i64)) as u32) * 2 - 1
    }
}

#[cfg(test)]
mod tests {
    use super::Bits;

    /// `append` copies whole bytes when it can; the result is the same as
    /// writing the bits one at a time, wherever either side ends.
    #[test]
    fn append_is_the_bits_in_order() {
        for lead in 0..9u32 {
            for tail in 0..8u32 {
                for bytes in [0usize, 1, 5] {
                    let mut o = Bits::default();
                    for i in 0..bytes {
                        o.put(8, (i * 37 + 0xA5) as u64 & 0xFF);
                    }
                    o.put(tail, 0x55 & ((1 << tail) - 1));
                    let (mut a, mut b) = (Bits::default(), Bits::default());
                    a.put(lead, 0x1B3 & ((1 << lead) - 1));
                    b.put(lead, 0x1B3 & ((1 << lead) - 1));
                    a.append(&o);
                    for i in 0..o.len() {
                        let byte = o.out.get(i / 8).map_or(o.acc as u8, |&v| v);
                        b.bit(byte >> (i % 8) & 1 == 1);
                    }
                    assert_eq!(a.len(), b.len(), "{lead} {tail} {bytes}");
                    a.pad();
                    b.pad();
                    assert_eq!(a.out, b.out, "{lead} {tail} {bytes}");
                }
            }
        }
    }
}
