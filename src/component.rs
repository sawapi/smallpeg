//! Per-component coefficient storage and the MCU interleave order.

use crate::COEF_MAX;
use crate::dct::fdct8x8;
use crate::quant::ZIGZAG;

pub(crate) struct Component {
    /// Quantized coefficients per block, already in zigzag order.
    pub(crate) blocks: Vec<[i16; 64]>,
    /// Pre-rounding coefficients (DCT value / quant step), zigzag order.
    /// Kept so trellis quantization can price alternative roundings.
    pub(crate) raw: Vec<[f32; 64]>,
    /// 0 = luma tables, 1 = chroma tables.
    pub(crate) table_class: usize,
    /// Width of the (MCU-padded) block grid `blocks` is stored in.
    pub(crate) bw: usize,
    /// Actual block count covering the component's samples. In 4:2:0 the
    /// luma grid is padded to whole MCUs; non-interleaved (progressive AC)
    /// scans must cover exactly these blocks and skip the padding.
    pub(crate) aw: usize,
    pub(crate) ah: usize,
    pub(crate) h: u8,
    pub(crate) v: u8,
    pub(crate) qt_id: u8,
    /// Pre-quantization DCT AC energy split (zigzag 1-2 vs all), i.e. a
    /// quant-table-independent gradient-vs-texture signal.
    pub(crate) e_low: f64,
    pub(crate) e_all: f64,
    /// Accumulate `e_low` / `e_all` in `push_block` (only the component
    /// `is_gradient` will judge needs them).
    pub(crate) energy: bool,
}

impl Component {
    pub(crate) fn with_grid(
        bw: usize,
        aw: usize,
        ah: usize,
        h: u8,
        v: u8,
        qt_id: u8,
        nblocks: usize,
    ) -> Component {
        Component {
            blocks: Vec::with_capacity(nblocks),
            raw: Vec::with_capacity(nblocks),
            table_class: if qt_id == 0 { 0 } else { 1 },
            bw,
            aw,
            ah,
            h,
            v,
            qt_id,
            e_low: 0.0,
            e_all: 0.0,
            energy: false,
        }
    }

    /// DCT + quantize one level-shifted 8x8 block and append it.
    pub(crate) fn push_block(&mut self, px: &mut [f32; 64], recip: &[f32; 64]) {
        let mut zz = [0i16; 64];
        let mut t = [0f32; 64];
        let mut energy = (self.e_low, self.e_all);
        dct_quant(
            px,
            recip,
            &mut zz,
            &mut t,
            self.energy.then_some(&mut energy),
        );
        (self.e_low, self.e_all) = energy;
        self.blocks.push(zz);
        self.raw.push(t);
    }
}

/// DCT + quantize one level-shifted 8x8 block into `zz` (quantized) and `t`
/// (pre-rounding), both in zigzag order. If `energy` is given, the block's
/// pre-quantization AC energy is added to it as (zigzag 1..=5, all).
pub(crate) fn dct_quant(
    px: &mut [f32; 64],
    recip: &[f32; 64],
    zz: &mut [i16; 64],
    t: &mut [f32; 64],
    energy: Option<&mut (f64, f64)>,
) {
    fdct8x8(px);
    for i in 0..64 {
        // Baseline 8-bit JPEG codes AC magnitudes up to 1023 (size 10)
        // and DC values in [-1024, 1023]. 8-bit samples stay inside
        // that by construction; this guards anything that could push
        // past it (samples beyond the clip points, rounding at step 1),
        // since strict decoders reject the whole file otherwise.
        t[i] = (px[ZIGZAG[i]] * recip[ZIGZAG[i]]).clamp(-COEF_MAX, COEF_MAX);
        zz[i] = t[i].round() as i16;
    }
    if let Some((e_low, e_all)) = energy {
        for i in 1..64 {
            let e = (px[ZIGZAG[i]] * px[ZIGZAG[i]]) as f64;
            *e_all += e;
            if i <= 5 {
                *e_low += e;
            }
        }
    }
}

/// Block emission order for an interleaved scan: per MCU, each component's
/// sampling region left-to-right, top-to-bottom.
pub(crate) fn interleave_order(
    comps: &[Component],
    mcu_w: usize,
    mcu_h: usize,
) -> Vec<(usize, usize)> {
    let mut order = Vec::new();
    for my in 0..mcu_h {
        for mx in 0..mcu_w {
            for (ci, c) in comps.iter().enumerate() {
                let bw = mcu_w * c.h as usize;
                for dy in 0..c.v as usize {
                    for dx in 0..c.h as usize {
                        let bx = mx * c.h as usize + dx;
                        let by = my * c.v as usize + dy;
                        order.push((ci, by * bw + bx));
                    }
                }
            }
        }
    }
    order
}
