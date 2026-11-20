//! Forward DCT (AAN scaled) and the matching quantization divisors.

/// One 8-point pass of the AAN (Arai–Agui–Nakajima 1988) scaled DCT:
/// 5 multiplications, 29 additions. Output k is the true DCT coefficient
/// times 2√2·(√2·cos(kπ/16)) (and times 2√2 for k = 0); the descaling is
/// folded into the quantization divisors, so it never costs anything.
#[inline]
fn aan_pass(x: [f32; 8]) -> [f32; 8] {
    const C4: f32 = std::f32::consts::FRAC_1_SQRT_2; // cos(4π/16)
    const C6: f32 = 0.38268343; // cos(6π/16)
    const C2M6: f32 = 0.5411961; // cos(2π/16) - cos(6π/16)
    const C2P6: f32 = 1.3065629; // cos(2π/16) + cos(6π/16)

    let tmp0 = x[0] + x[7];
    let tmp7 = x[0] - x[7];
    let tmp1 = x[1] + x[6];
    let tmp6 = x[1] - x[6];
    let tmp2 = x[2] + x[5];
    let tmp5 = x[2] - x[5];
    let tmp3 = x[3] + x[4];
    let tmp4 = x[3] - x[4];

    // Even part.
    let tmp10 = tmp0 + tmp3;
    let tmp13 = tmp0 - tmp3;
    let tmp11 = tmp1 + tmp2;
    let tmp12 = tmp1 - tmp2;
    let z1 = (tmp12 + tmp13) * C4;

    // Odd part.
    let t10 = tmp4 + tmp5;
    let t11 = tmp5 + tmp6;
    let t12 = tmp6 + tmp7;
    let z5 = (t10 - t12) * C6;
    let z2 = C2M6 * t10 + z5;
    let z4 = C2P6 * t12 + z5;
    let z3 = t11 * C4;
    let z11 = tmp7 + z3;
    let z13 = tmp7 - z3;

    [
        tmp10 + tmp11,
        z11 + z4,
        tmp13 + z1,
        z13 - z2,
        tmp10 - tmp11,
        z13 + z2,
        tmp13 - z1,
        z11 - z4,
    ]
}

/// In-place 2D scaled DCT: AAN over rows, then over columns.
pub(crate) fn fdct8x8(block: &mut [f32; 64]) {
    for row in block.as_chunks_mut::<8>().0 {
        *row = aan_pass(*row);
    }
    for x in 0..8 {
        let mut col = [0f32; 8];
        for (y, c) in col.iter_mut().enumerate() {
            *c = block[y * 8 + x];
        }
        let out = aan_pass(col);
        for (y, &v) in out.iter().enumerate() {
            block[y * 8 + x] = v;
        }
    }
}

/// Combined dequantization + AAN descaling multipliers, natural order:
/// recip[i] = 1 / (qt[i] * 8 * s(row) * s(col)) with s(0) = 1 and
/// s(k) = √2·cos(kπ/16), so that `coeff * recip` equals DCT/qt exactly as
/// the naive orthonormal transform produced it.
pub(crate) fn dequant_scale(qt: &[u16; 64]) -> [f32; 64] {
    let mut s = [0f32; 8];
    for (k, v) in s.iter_mut().enumerate() {
        *v = if k == 0 {
            1.0
        } else {
            std::f32::consts::SQRT_2 * (k as f32 * std::f32::consts::PI / 16.0).cos()
        };
    }
    let mut out = [0f32; 64];
    for (i, o) in out.iter_mut().enumerate() {
        *o = 1.0 / (qt[i] as f32 * 8.0 * s[i / 8] * s[i % 8]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The AAN transform, descaled through dequant_scale with a unit quant
    /// table, must match a naive orthonormal DCT (the JPEG-standard one).
    #[test]
    fn aan_matches_naive_dct() {
        let mut basis = [[0f32; 8]; 8];
        for (k, row) in basis.iter_mut().enumerate() {
            let a = if k == 0 { (1f32 / 8.0).sqrt() } else { 0.5 };
            for (n, v) in row.iter_mut().enumerate() {
                *v = a * ((2 * n + 1) as f32 * k as f32 * std::f32::consts::PI / 16.0).cos();
            }
        }
        let mut blk = [0f32; 64];
        for (i, v) in blk.iter_mut().enumerate() {
            *v = ((i * 37 + i * i * 11) % 256) as f32 - 128.0;
        }

        let mut naive = [0f32; 64];
        for u in 0..8 {
            for v in 0..8 {
                let mut acc = 0f32;
                for y in 0..8 {
                    for x in 0..8 {
                        acc += blk[y * 8 + x] * basis[u][y] * basis[v][x];
                    }
                }
                naive[u * 8 + v] = acc;
            }
        }

        let mut aan = blk;
        fdct8x8(&mut aan);
        let recip = dequant_scale(&[1u16; 64]);
        for i in 0..64 {
            let got = aan[i] * recip[i];
            assert!(
                (got - naive[i]).abs() < 0.01,
                "coeff {i}: aan {got} vs naive {}",
                naive[i]
            );
        }
    }
}
