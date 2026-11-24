//! The frame: block contexts, coefficient orders, and the sections of the
//! codestream.

use std::borrow::Cow;

use super::Meta;
use super::bits::{Bits, ceil_log2, pack_signed};
use super::entropy::{Tokens, hybrid, write_context_map, write_histograms};
use super::icc::write_icc;
use super::modular::{
    Node, PRED_GRADIENT, PRED_WEIGHTED, PRED_ZERO, PROP_STREAM, Tree, dc_tree, hist_cost,
    modular_header, split, stream_tokens,
};
use crate::optimize::Parsed;
use crate::quant::ZIGZAG;

const NON_ZERO_BUCKETS: u32 = 37;
const ZERO_DENSITY_CONTEXTS: u32 = 458;
const NUM_ORDERS: usize = 13;

/// Block contexts: blocks split by their luma DC value into up to eight
/// buckets, which each channel groups into as few contexts as pay off.
struct BlockCtx {
    /// The fine buckets: a block's counts the thresholds below its DC.
    fine: Vec<i32>,
    /// Luma DC thresholds as coded.
    thresholds: Vec<i32>,
    /// Context of (channel in Y, Cb, Cr order, bucket) for DCT8.
    pub(super) map: Vec<u8>,
    num: u32,
}

/// Luma DC values splitting the blocks into up to eight equal parts.
fn fine_thresholds(luma_dc: &[i16]) -> Vec<i32> {
    let mut v: Vec<i32> = luma_dc.iter().map(|&d| d as i32).collect();
    v.sort_unstable();
    let mut t: Vec<i32> = Vec::new();
    for j in 1..8 {
        let Some(&x) = v.get(v.len() * j / 8) else {
            break;
        };
        // A block is above a threshold when its DC is greater.
        if t.last().is_none_or(|&l| x - 1 > l) && x > v[0] {
            t.push(x - 1);
        }
    }
    t
}

impl BlockCtx {
    /// `ks`: the number of contexts for Y, Cb and Cr, each a power of two
    /// no larger than the number of fine buckets.
    pub(super) fn new(fine: Vec<i32>, ks: [usize; 3]) -> Self {
        let f = fine.len() + 1;
        let k = *ks.iter().max().unwrap();
        let thresholds: Vec<i32> = (1..f)
            .filter(|&i| i * k / f != (i - 1) * k / f)
            .map(|i| fine[i - 1])
            .collect();
        debug_assert_eq!(thresholds.len() + 1, k);
        let mut map = vec![0u8; 3 * NUM_ORDERS * k];
        let mut off = 0;
        for (ch, &kc) in ks.iter().enumerate() {
            for j in 0..k {
                map[ch * NUM_ORDERS * k + j] = (off + j * kc / k) as u8;
            }
            off += kc;
        }
        BlockCtx {
            fine,
            thresholds,
            map,
            num: off as u32,
        }
    }

    /// The fine bucket of a luma DC value.
    fn bucket(&self, dc: i32) -> usize {
        self.fine.iter().filter(|&&t| dc > t).count()
    }

    pub(super) fn write(&self, w: &mut Bits) {
        w.bit(false); // not the default map
        for c in 0..3 {
            let t: &[i32] = if c == 1 { &self.thresholds } else { &[] };
            w.put(4, t.len() as u64);
            for &v in t {
                w.u32(pack_signed(v), [(0, 4), (16, 8), (272, 16), (65808, 32)]);
            }
        }
        w.put(4, 0); // no quant field thresholds
        write_context_map(w, &self.map);
    }

    /// Context of a block of JPEG XL channel `c` (Cb, Y, Cr) in fine
    /// bucket `fb`.
    pub(super) fn get(&self, c: usize, fb: usize) -> u32 {
        let k = self.thresholds.len() + 1;
        let ch = if c < 2 { c ^ 1 } else { 2 };
        self.map[ch * NUM_ORDERS * k + fb * k / (self.fine.len() + 1)] as u32
    }
}
/// An AC token before the block contexts are chosen, in four bytes (they
/// are most of what a conversion holds in memory): its context within a
/// block context (nonzero counts first, then coefficients), its place
/// (channel and fine bucket, as `channel * 8 + bucket`), and its value.
#[derive(Clone, Copy)]
struct AcToken(u32);

impl AcToken {
    fn new(local: u32, place: u8, value: u32) -> Self {
        debug_assert!(local < 1 << 9 && place < 1 << 5 && value < 1 << 18);
        AcToken(local | (place as u32) << 9 | value << 14)
    }

    fn local(self) -> u32 {
        self.0 & 0x1FF
    }

    fn place(self) -> usize {
        (self.0 >> 9 & 0x1F) as usize
    }

    fn value(self) -> u32 {
        self.0 >> 14
    }

    /// As (context, value) under the block contexts `bctx`.
    fn coded(self, bctx: &BlockCtx) -> (u32, u32) {
        let bc = bctx.get(self.place() / 8, self.place() % 8);
        let local = self.local();
        let ctx = if local < NON_ZERO_BUCKETS {
            local * bctx.num + bc
        } else {
            bctx.num * NON_ZERO_BUCKETS + ZERO_DENSITY_CONTEXTS * bc + (local - NON_ZERO_BUCKETS)
        };
        (ctx, self.value())
    }
}

/// How many block contexts each channel gets (Y, Cb, Cr order): the
/// grouping of the `f` fine buckets with the least estimated cost, within
/// the format's limit of 16 in all. `ac` holds tokens with local
/// contexts, `wh` each one's JPEG XL channel (Cb, Y, Cr) and fine bucket.
fn choose_block_ctx(ac: &[Vec<AcToken>], f: usize) -> [usize; 3] {
    const LOCAL: usize = (NON_ZERO_BUCKETS + ZERO_DENSITY_CONTEXTS) as usize;
    let mut hist = vec![[0u32; 64]; 3 * 8 * LOCAL];
    for t in ac.iter().flatten() {
        hist[t.place() * LOCAL + t.local() as usize][hybrid(t.value()).0.min(63)] += 1;
    }
    let cost = |h: &[u32; 64]| {
        let mut wide = [0u32; 128];
        wide[..64].copy_from_slice(h);
        hist_cost(&wide)
    };
    let ks: Vec<usize> = [1, 2, 4, 8].into_iter().filter(|&k| k <= f).collect();
    // costs[JPEG XL channel][index into ks]
    let costs: Vec<Vec<f64>> = (0..3)
        .map(|c| {
            ks.iter()
                .map(|&k| {
                    let mut total = 0.0;
                    for g in 0..k {
                        for l in 0..LOCAL {
                            let mut h = [0u32; 64];
                            for b in (0..f).filter(|&b| b * k / f == g) {
                                for (x, y) in h.iter_mut().zip(&hist[(c * 8 + b) * LOCAL + l]) {
                                    *x += y;
                                }
                            }
                            total += cost(&h);
                        }
                    }
                    total
                })
                .collect()
        })
        .collect();
    let mut best = (f64::MAX, [1; 3]);
    for (iy, &y) in ks.iter().enumerate() {
        for (ib, &cb) in ks.iter().enumerate() {
            for (ir, &cr) in ks.iter().enumerate() {
                let c = costs[1][iy] + costs[0][ib] + costs[2][ir];
                if y + cb + cr <= 16 && c < best.0 {
                    best = (c, [y, cb, cr]);
                }
            }
        }
    }
    best.1
}

const COEFF_FREQ_CONTEXT: [u32; 64] = [
    0, 0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 30, 32, 32, 34, 34, 36, 36, 38,
    38, 40, 40, 42, 42, 44, 44, 46, 46, 46, 46, 48, 48, 48, 48, 50, 50, 50, 50, 52, 52, 52, 52, 54,
    54, 54, 54, 56, 56, 56, 56, 58, 58, 58, 58, 60, 60, 60, 60,
];
const COEFF_NUM_NONZERO_CONTEXT: [u32; 64] = [
    0, 0, 62, 124, 124, 186, 186, 186, 186, 246, 246, 246, 246, 304, 304, 304, 304, 304, 304, 304,
    304, 360, 360, 360, 360, 360, 360, 360, 360, 360, 360, 360, 360, 412, 412, 412, 412, 412, 412,
    412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412, 412,
    412, 412, 412, 412, 412, 412,
];

fn nonzero_context(predicted: usize, block_ctx: u32, num_ctx: u32) -> u32 {
    let b = if predicted < 8 {
        predicted
    } else if predicted < 64 {
        4 + predicted / 2
    } else {
        36
    };
    b as u32 * num_ctx + block_ctx
}

struct Chan<'a> {
    hs: usize,
    vs: usize,
    /// Blocks of the JPEG component, row stride `bw`, zigzag order; none
    /// for the chroma of a grayscale image, which is all zeros. Owned
    /// when the frame's grid is larger than the component's.
    blocks: Option<Cow<'a, [[i16; 64]]>>,
    bw: usize,
    /// Quantization table in natural order.
    q: [u16; 64],
}

impl Chan<'_> {
    fn blk(&self, i: usize) -> &[i16; 64] {
        const ZERO: [i16; 64] = [0; 64];
        match &self.blocks {
            Some(b) => &b[i],
            None => &ZERO,
        }
    }
}

pub(super) struct Frame<'a> {
    pub(super) width: usize,
    pub(super) height: usize,
    /// Channels in JPEG XL order: Cb, Y, Cr. A grayscale image is its
    /// one component as Y, with empty chroma of the same shape.
    ch: [Chan<'a>; 3],
    gray: bool,
    up: [u32; 3],
    xblocks: usize,
    yblocks: usize,
}

/// The frame's sampling mode for a component with these sampling factors.
pub(super) fn sampling_mode(h: usize, v: usize) -> Option<u32> {
    match (h, v) {
        (1, 1) => Some(0),
        (2, 2) => Some(1),
        (2, 1) => Some(2),
        (1, 2) => Some(3),
        _ => None,
    }
}

/// The sampling factors a one-component JPEG's frame header gives. They
/// change nothing in the image, and the parser takes them as 1x1; only
/// writing the header back needs them.
pub(super) fn gray_sampling(data: &[u8], p: &Parsed) -> (usize, usize) {
    // SOF: marker, length, precision, height, width, count, then per
    // component its id, sampling factors and table.
    let hv = data.get(p.frame_at + 11).copied().unwrap_or(0x11);
    ((hv >> 4) as usize, (hv & 15) as usize)
}

impl<'a> Frame<'a> {
    /// The frame for `p`. A grayscale image takes `gray_hv` as its
    /// sampling factors: 1x1, or the frame header's own so that a decoder
    /// rebuilding the JPEG writes them back.
    pub(super) fn new(p: &'a Parsed, gray_hv: (usize, usize)) -> Option<Self> {
        let gray = p.comps.len() == 1;
        if !gray {
            if p.comps.len() != 3 || p.adobe == Some(0) {
                return None;
            }
            if !p.jfif && p.adobe.is_none() && p.ids == *b"RGB" {
                return None;
            }
        }
        const HS: [usize; 4] = [0, 1, 1, 0];
        const VS: [usize; 4] = [0, 1, 0, 1];
        let jpeg_of = if gray { [0usize; 3] } else { [1, 0, 2] };
        let mut up = [0u32; 3];
        for c in 0..3 {
            let j = &p.comps[jpeg_of[c]];
            let (h, v) = if gray { gray_hv } else { (j.h, j.v) };
            up[c] = sampling_mode(h, v)?;
        }
        let maxhs = up.iter().map(|&m| HS[m as usize]).max().unwrap();
        let maxvs = up.iter().map(|&m| VS[m as usize]).max().unwrap();
        let (width, height) = (p.width as usize, p.height as usize);
        let xblocks = width.div_ceil(8 << maxhs) << maxhs;
        let yblocks = height.div_ceil(8 << maxvs) << maxvs;
        let mut chans = Vec::new();
        for c in 0..3 {
            let j = &p.comps[jpeg_of[c]];
            let hs = maxhs - HS[up[c] as usize];
            let vs = maxvs - VS[up[c] as usize];
            let qz = p.qts[j.tq as usize].as_ref()?;
            // A zero step is no quantizer a JPEG XL decoder accepts.
            if qz.contains(&0) {
                return None;
            }
            let mut q = [0u16; 64];
            for k in 0..64 {
                q[ZIGZAG[k]] = qz[k];
            }
            let (fw, fh) = (xblocks >> hs, yblocks >> vs);
            let (blocks, bw) = if gray && c != 1 {
                (None, fw)
            } else if j.bw >= fw && j.blocks.len() >= fh * j.bw {
                (Some(Cow::Borrowed(&j.blocks[..])), j.bw)
            } else if gray && j.aw > 0 && j.ah > 0 && j.blocks.len() >= j.ah * j.bw {
                // The frame is whole MCUs of the header's sampling factors,
                // the component only the blocks its samples need. The
                // blocks beyond them are in no scan, so they are free.
                // A decoder smooths the DC image except at its border and
                // where a neighbor is far off, so each gets a DC far
                // outside the image's range: the last real row and column
                // then stay as they are, as at the border of the frame
                // without padding. (Up to the range of a coefficient;
                // past that the edge is smoothed a little.)
                let dcs = || {
                    (0..j.ah).flat_map(|y| (0..j.aw).map(move |x| j.blocks[y * j.bw + x][0] as i32))
                };
                let (lo, hi) = (dcs().min()?, dcs().max()?);
                let far = (hi + 3 + 3 * (hi - lo)).min(i16::MAX as i32) as i16;
                let mut padded = Vec::with_capacity(fw * fh);
                for y in 0..fh {
                    for x in 0..fw {
                        padded.push(if x < j.aw && y < j.ah {
                            j.blocks[y * j.bw + x]
                        } else {
                            let mut b = [0i16; 64];
                            b[0] = far;
                            b
                        });
                    }
                }
                (Some(Cow::Owned(padded)), fw)
            } else {
                // The block grid must be the one JPEG XL derives.
                return None;
            };
            chans.push(Chan {
                hs,
                vs,
                blocks,
                bw,
                q,
            });
        }
        let ch: [Chan<'a>; 3] = chans.try_into().ok()?;
        Some(Frame {
            width,
            height,
            ch,
            gray,
            up,
            xblocks,
            yblocks,
        })
    }

    fn is444(&self) -> bool {
        self.ch.iter().all(|c| c.hs == 0 && c.vs == 0)
    }

    pub(super) fn encode(&self, meta: &Meta) -> Vec<u8> {
        let mut w = Bits::default();
        self.headers(&mut w, meta);
        self.frame_header(&mut w);

        let groups_x = self.width.div_ceil(256);
        let groups_y = self.height.div_ceil(256);
        let num_groups = groups_x * groups_y;
        let lf_x = self.xblocks.div_ceil(256);
        let lf_y = self.yblocks.div_ceil(256);
        let num_lf = lf_x * lf_y;

        // Modular streams, all through one global tree: by stream id the
        // quant table, the block metadata (all zero) and the DC.
        // The DC planes of each LF group, in Cb, Y, Cr order.
        let mut lf_rects = Vec::new();
        let mut dc_planes = Vec::new();
        for g in 0..num_lf {
            let (ox, oy) = ((g % lf_x) * 256, (g / lf_x) * 256);
            let (bw, bh) = ((self.xblocks - ox).min(256), (self.yblocks - oy).min(256));
            lf_rects.push((bw, bh));
            let mut planes = Vec::new();
            for c in [1usize, 0, 2] {
                let ch = &self.ch[c];
                let (w, h) = (bw >> ch.hs, bh >> ch.vs);
                let (x0, y0) = (ox >> ch.hs, oy >> ch.vs);
                let mut v = Vec::with_capacity(w * h);
                for y in 0..h {
                    for x in 0..w {
                        v.push(ch.blk((y0 + y) * ch.bw + x0 + x)[0] as i32);
                    }
                }
                planes.push((v, w, h));
            }
            dc_planes.push(planes);
        }
        // Run the weighted predictor over the DC once to learn its tree.
        let probe = Tree::new(Node::Leaf {
            pred: PRED_WEIGHTED,
        });
        let mut samples = Vec::new();
        for planes in &dc_planes {
            let chans: Vec<(&[i32], usize, usize)> =
                planes.iter().map(|(v, w, h)| (&v[..], *w, *h)).collect();
            let (mut t, mut trace) = (Tokens::new(), Vec::new());
            stream_tokens(&probe, 0, &chans, &mut t, Some(&mut trace));
            samples.extend(
                trace
                    .iter()
                    .zip(&t)
                    .map(|(&(c, p), &(_, v))| (c, p, hybrid(v).0 as u8)),
            );
        }
        let n = num_lf as i32;
        let tree = Tree::new(split(
            PROP_STREAM,
            n,
            split(
                PROP_STREAM,
                3 * n,
                Node::Leaf {
                    pred: PRED_GRADIENT,
                },
                Node::Leaf { pred: PRED_ZERO },
            ),
            dc_tree(&mut samples, 6),
        ));
        let mut lf_tokens: Vec<Tokens> = Vec::new();
        let mut meta_tokens: Vec<Tokens> = Vec::new();
        for (g, planes) in dc_planes.iter().enumerate() {
            let (bw, bh) = lf_rects[g];
            let mut t = Tokens::new();
            let chans: Vec<(&[i32], usize, usize)> =
                planes.iter().map(|(v, w, h)| (&v[..], *w, *h)).collect();
            stream_tokens(&tree, 1 + g as u32, &chans, &mut t, None);
            lf_tokens.push(t);
            // Chroma-from-luma maps, block types and quant field, smoothing:
            // all zero (no CfL, DCT8 everywhere, unit quantizer).
            let (cw, chh) = (bw.div_ceil(8), bh.div_ceil(8));
            let (z1, z2, z3) = (vec![0; cw * chh], vec![0; 2 * bw * bh], vec![0; bw * bh]);
            let mut t = Tokens::new();
            let meta: [(&[i32], usize, usize); 4] = [
                (&z1, cw, chh),
                (&z1, cw, chh),
                (&z2, bw * bh, 2),
                (&z3, bw, bh),
            ];
            stream_tokens(&tree, 1 + 2 * num_lf as u32 + g as u32, &meta, &mut t, None);
            meta_tokens.push(t);
        }
        let mut qt = Vec::with_capacity(192);
        for c in 0..3 {
            for y in 0..8 {
                for x in 0..8 {
                    // JPEG XL's DCT is the transpose of JPEG's.
                    qt.push(self.ch[c].q[8 * x + y] as i32);
                }
            }
        }
        let mut qt_tokens = Tokens::new();
        let qchans: Vec<(&[i32], usize, usize)> =
            (0..3).map(|c| (&qt[64 * c..64 * (c + 1)], 8, 8)).collect();
        stream_tokens(&tree, 1 + 3 * num_lf as u32, &qchans, &mut qt_tokens, None);

        // ---- LfGlobal
        let mut lfg = Bits::default();
        lfg.bit(false); // LF dequantization: custom
        for c in 0..3 {
            let dcq = 255.0f32 * 8.0 / self.ch[c].q[0] as f32;
            lfg.f16((1.0f32 / dcq) * 128.0);
        }
        lfg.put(2, 3); // global scale 65536: unit quantizer
        lfg.put(16, 65536 - 8193);
        lfg.put(2, 1); // quant_lf 1
        lfg.put(5, 0);
        let luma = &self.ch[1];
        let mut luma_dc = Vec::new();
        for y in 0..self.yblocks >> luma.vs {
            for x in 0..self.xblocks >> luma.hs {
                luma_dc.push(luma.blk(y * luma.bw + x)[0]);
            }
        }
        let fine = fine_thresholds(&luma_dc);
        // ---- AC tokens per group
        // Coefficient orders: positions that are more often zero go
        // later, per channel, so blocks end sooner.
        let nat = natural_order();
        let to_zz = jxl_to_zigzag();
        let mut perms = [[0usize; 64]; 3];
        for c in 0..3 {
            let ch = &self.ch[c];
            let mut zeros = [0u64; 64];
            for y in 0..self.yblocks >> ch.vs {
                for x in 0..self.xblocks >> ch.hs {
                    let blk = ch.blk(y * ch.bw + x);
                    for (pos, z) in zeros.iter_mut().enumerate() {
                        *z += (blk[to_zz[pos]] == 0) as u64;
                    }
                }
            }
            // Ties keep the natural order (a stable sort); DC stays first.
            let key = |i: usize| if i == 0 { 0 } else { zeros[nat[i]] + 1 };
            let mut p: [usize; 64] = std::array::from_fn(|i| i);
            p.sort_by_key(|&i| key(i));
            perms[c] = p;
        }
        let custom = perms
            .iter()
            .any(|p| p.iter().enumerate().any(|(i, &v)| i != v));
        let orders: [[usize; 64]; 3] =
            std::array::from_fn(|c| std::array::from_fn(|k| to_zz[nat[perms[c][k]]]));
        // AC tokens with contexts local to a block context, and each one's
        // channel and fine bucket; the block contexts are chosen from them
        // below.
        let fb = BlockCtx::new(fine.clone(), [1; 3]);
        let mut ac: Vec<Vec<AcToken>> = Vec::new();
        for g in 0..num_groups {
            let (ox, oy) = ((g % groups_x) * 32, (g / groups_x) * 32);
            let (gw, gh) = ((self.xblocks - ox).min(32), (self.yblocks - oy).min(32));
            let mut t = Vec::new();
            let mut nz = [[0u8; 32]; 3];
            for by in 0..gh {
                for bx in 0..gw {
                    for c in [1usize, 0, 2] {
                        let ch = &self.ch[c];
                        let (sbx, sby) = (bx >> ch.hs, by >> ch.vs);
                        if sbx << ch.hs != bx || sby << ch.vs != by {
                            continue;
                        }
                        let predicted = if sbx == 0 {
                            if sby == 0 { 32 } else { nz[c][0] as usize }
                        } else if sby == 0 {
                            nz[c][sbx - 1] as usize
                        } else {
                            (nz[c][sbx] as usize + nz[c][sbx - 1] as usize).div_ceil(2)
                        };
                        let blk = ch.blk(((oy >> ch.vs) + sby) * ch.bw + (ox >> ch.hs) + sbx);
                        let coef = |k: usize| blk[orders[c][k]] as i32;
                        let nonzeros = (1..64).filter(|&k| coef(k) != 0).count();
                        let dc =
                            luma.blk(((oy + by) >> luma.vs) * luma.bw + ((ox + bx) >> luma.hs))[0];
                        let at = (c * 8 + fb.bucket(dc as i32)) as u8;
                        t.push(AcToken::new(
                            nonzero_context(predicted, 0, 1),
                            at,
                            nonzeros as u32,
                        ));
                        nz[c][sbx] = nonzeros as u8;
                        if nonzeros == 0 {
                            continue;
                        }
                        let base = NON_ZERO_BUCKETS;
                        let mut left = nonzeros;
                        let mut prev = if nonzeros > 4 { 0 } else { 1 };
                        for k in 1..64 {
                            let v = coef(k);
                            let ctx = base
                                + COEFF_NUM_NONZERO_CONTEXT[left]
                                + COEFF_FREQ_CONTEXT[k]
                                + prev;
                            t.push(AcToken::new(ctx, at, pack_signed(v)));
                            if v == 0 {
                                prev = 0;
                                continue;
                            }
                            left -= 1;
                            if left == 0 {
                                break;
                            }
                            prev = 1;
                        }
                    }
                }
            }
            // Kept until the end: give back what growing it left over.
            t.shrink_to_fit();
            ac.push(t);
        }
        let bctx = BlockCtx::new(fine, choose_block_ctx(&ac, fb.fine.len() + 1));
        bctx.write(&mut lfg);
        lfg.bit(false); // color correlation: none (YCbCr)
        lfg.put(2, 0);
        lfg.put(16, 0);
        lfg.put(16, 0);
        lfg.put(8, 128);
        lfg.put(8, 128);
        lfg.bit(true); // global tree
        let tree_tokens = tree.tokens();
        let tc = write_histograms(&mut lfg, 6, tree_tokens.iter().copied());
        tc.put_all(&mut lfg, tree_tokens.iter().copied());
        let all = std::iter::once(&qt_tokens)
            .chain(&lf_tokens)
            .chain(&meta_tokens)
            .flatten()
            .copied();
        let mc = write_histograms(&mut lfg, tree.leaves, all);

        // ---- LF groups
        let mut lf_sections = Vec::new();
        for g in 0..num_lf {
            let mut s = Bits::default();
            s.put(2, 0); // no extra precision
            modular_header(&mut s);
            mc.put_all(&mut s, lf_tokens[g].iter().copied());
            let (bw, bh) = lf_rects[g];
            s.put(ceil_log2(bw * bh), (bw * bh - 1) as u64);
            modular_header(&mut s);
            mc.put_all(&mut s, meta_tokens[g].iter().copied());
            lf_sections.push(s);
        }

        // ---- HfGlobal
        let mut hfg = Bits::default();
        hfg.bit(false); // custom dequantization matrices
        for i in 0..17 {
            if i == 0 {
                hfg.put(3, 7); // raw table for DCT8: the JPEG's own
                hfg.f16(1.0f32 / (8.0 * 255.0));
                modular_header(&mut hfg);
                mc.put_all(&mut hfg, qt_tokens.iter().copied());
            } else {
                hfg.put(3, 0); // library default (unused)
            }
        }
        hfg.put(ceil_log2(num_groups), 0); // one histogram set
        if custom {
            hfg.put(2, 3);
            hfg.put(13, 1); // an order for 8x8 DCT
            let mut t = Tokens::new();
            for p in &perms {
                permutation_tokens(p, &mut t);
            }
            let code = write_histograms(&mut hfg, 8, t.iter().copied());
            code.put_all(&mut hfg, t.iter().copied());
        } else {
            hfg.put(2, 2); // natural coefficient orders
        }
        let num_ac = bctx.num * (NON_ZERO_BUCKETS + ZERO_DENSITY_CONTEXTS);
        let all_ac = ac.iter().flatten().map(|t| t.coded(&bctx));
        let acc = write_histograms(&mut hfg, num_ac as usize, all_ac);

        // ---- pass groups
        // Each group's tokens are let go once they are coded.
        let mut pass_sections = Vec::new();
        for t in ac {
            let mut s = Bits::default();
            acc.put_all(&mut s, t.iter().map(|t| t.coded(&bctx)));
            pass_sections.push(s);
        }

        // ---- TOC and sections
        let mut sections = vec![lfg];
        sections.extend(lf_sections);
        sections.push(hfg);
        sections.extend(pass_sections);
        w.bit(false); // TOC not permuted
        w.pad();
        if num_groups == 1 {
            let mut one = Bits::default();
            for s in &sections {
                one.append(s);
            }
            one.pad();
            w.u32(one.out.len() as u32, TOC_DIST);
            w.pad();
            w.append(&one);
        } else {
            for s in sections.iter_mut() {
                s.pad();
                w.u32(s.out.len() as u32, TOC_DIST);
            }
            w.pad();
            // Room for all of them at once, and each let go as it is copied.
            w.out.reserve(sections.iter().map(|s| s.out.len()).sum());
            for s in sections {
                w.append(&s);
            }
        }
        w.pad();
        w.out
    }

    fn headers(&self, w: &mut Bits, meta: &Meta) {
        w.put(8, 0xFF);
        w.put(8, 0x0A);
        const SIZE: [(u32, u32); 4] = [(1, 9), (1, 13), (1, 18), (1, 30)];
        w.bit(false); // not a small size
        w.u32(self.height as u32, SIZE);
        w.put(3, 0); // no aspect ratio shortcut
        w.u32(self.width as u32, SIZE);
        w.bit(false); // metadata not all default
        let extra = meta.orientation != 1;
        w.bit(extra);
        if extra {
            w.put(3, meta.orientation as u64 - 1);
            w.bit(false); // no intrinsic size
            w.bit(false); // no preview
            w.bit(false); // no animation
        }
        w.bit(false); // integer samples
        w.put(2, 0); // 8 bits
        w.bit(true); // 16-bit buffers suffice
        w.put(2, 0); // no extra channels
        w.bit(false); // not XYB
        if meta.icc.is_some() {
            w.bit(false); // color encoding not all default
            w.bit(true); // an ICC profile
            w.put(2, self.gray as u64); // RGB or gray
        } else if self.gray {
            w.bit(false); // color encoding not all default
            w.bit(false); // no ICC profile
            w.put(2, 1); // gray
            w.put(2, 1); // white point D65
            w.bit(false); // no gamma: a named transfer function,
            w.put(2, 2);
            w.put(4, 13 - 2); // sRGB's
            w.put(2, 1); // rendering intent: relative
        } else {
            w.bit(true); // sRGB
        }
        if extra {
            w.bit(true); // default tone mapping
        }
        w.u64(0); // extensions
        w.bit(true); // default transform data
        if let Some(icc) = &meta.icc {
            write_icc(w, icc);
        }
        w.pad();
    }

    fn frame_header(&self, w: &mut Bits) {
        w.bit(false); // not all default
        w.put(2, 0); // regular frame
        w.put(1, 0); // VarDCT
        // Adaptive LF smoothing only at 4:4:4, as for other JPEG
        // recompression.
        w.u64(if self.is444() { 0 } else { 0x80 });
        w.bit(true); // YCbCr
        for &u in &self.up {
            w.put(2, u as u64);
        }
        w.put(2, 0); // no upsampling
        w.put(2, 0); // one pass
        w.bit(false); // no crop
        w.put(2, 0); // blend mode: replace
        w.bit(true); // last frame
        w.put(2, 0); // no name
        w.bit(false); // restoration filter not all default
        w.bit(false); // no Gabor-like smoothing
        w.put(2, 0); // no edge-preserving filter
        w.u64(0); // restoration filter extensions
        w.u64(0); // frame header extensions
    }
}

const TOC_DIST: [(u32, u32); 4] = [(0, 10), (1024, 14), (17408, 22), (4211712, 30)];

/// JPEG XL's natural coefficient order for 8x8 DCT, as coefficient
/// indices (column * 8 + row of a JPEG block).
fn natural_order() -> [usize; 64] {
    let mut out = [0usize; 64];
    let mut n = 0;
    for i in 0..8 {
        for j in 0..=i {
            let (mut x, mut y) = (j, i - j);
            if i % 2 == 1 {
                std::mem::swap(&mut x, &mut y);
            }
            out[n] = y * 8 + x;
            n += 1;
        }
    }
    for ip in (1..8).rev() {
        let i = ip - 1;
        for j in 0..=i {
            let (mut x, mut y) = (7 - (i - j), 7 - j);
            if i % 2 == 1 {
                std::mem::swap(&mut x, &mut y);
            }
            out[n] = y * 8 + x;
            n += 1;
        }
    }
    out
}

/// The zigzag index in a JPEG block of each JPEG XL coefficient index.
fn jxl_to_zigzag() -> [usize; 64] {
    let mut inv = [0usize; 64];
    for k in 0..64 {
        inv[ZIGZAG[k]] = k;
    }
    std::array::from_fn(|i| inv[(i % 8) * 8 + i / 8])
}

/// Context of a coefficient order symbol after `x`.
fn perm_context(x: u32) -> u32 {
    ceil_log2(x as usize + 1).min(7)
}

/// A coefficient order (positions into the natural order, the first
/// fixed) as a Lehmer code: its symbols, trailing zeros dropped.
fn permutation_tokens(perm: &[usize; 64], out: &mut Tokens) {
    let mut left: Vec<usize> = (1..64).collect();
    let mut code = Vec::with_capacity(63);
    for &p in &perm[1..] {
        let i = left.iter().position(|&v| v == p).unwrap();
        left.remove(i);
        code.push(i as u32);
    }
    let end = code.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
    out.push((perm_context(64), end as u32));
    let mut prev = 0;
    for &c in &code[..end] {
        out.push((perm_context(prev), c));
        prev = c;
    }
}

#[cfg(test)]
mod tests {
    use super::{AcToken, Frame};
    use crate::optimize::{Limits, parse_and_decode};

    /// A grayscale frame of whole MCUs has blocks the component does not:
    /// they are flat, and far enough above the image's DC values that a
    /// decoder's smoothing leaves the real blocks beside them alone.
    #[test]
    fn padding_of_a_gray_frame_is_far_from_the_image() {
        let gray: Vec<u8> = (0..150 * 91)
            .map(|i| (i % 150 + i / 150 * 2) as u8)
            .collect();
        let jpeg = crate::Encoder::new(90).encode_gray(&gray, 150, 91);
        let p = parse_and_decode(&jpeg, false, &Limits::default()).unwrap();
        let c = &p.comps[0];
        assert_eq!((c.aw, c.ah), (19, 12));
        let real = |x: usize, y: usize| c.blocks[y * c.bw + x];
        let dcs: Vec<i32> = (0..c.ah)
            .flat_map(|y| (0..c.aw).map(move |x| (x, y)))
            .map(|(x, y)| real(x, y)[0] as i32)
            .collect();
        let (lo, hi) = (*dcs.iter().min().unwrap(), *dcs.iter().max().unwrap());
        assert!(hi > lo);
        for (hv, fw, fh) in [((2, 1), 20, 12), ((1, 2), 19, 12), ((2, 2), 20, 12)] {
            let f = Frame::new(&p, hv).unwrap();
            let y = &f.ch[1];
            let blocks = y.blocks.as_ref().unwrap();
            assert_eq!((f.xblocks, f.yblocks, y.bw), (fw, fh, fw), "{hv:?}");
            for by in 0..fh {
                for bx in 0..fw {
                    let b = blocks[by * fw + bx];
                    if bx < c.aw && by < c.ah {
                        assert_eq!(b, real(bx, by), "{hv:?} {bx} {by}");
                    } else {
                        assert!(b[1..].iter().all(|&v| v == 0), "{hv:?} {bx} {by}");
                        assert!(
                            b[0] as i32 >= hi + 3 + 3 * (hi - lo),
                            "{hv:?} {bx} {by}: {}",
                            b[0]
                        );
                    }
                }
            }
        }
        // Nothing to pad at 1x1: the component's own blocks.
        assert!(matches!(
            Frame::new(&p, (1, 1)).unwrap().ch[1].blocks,
            Some(std::borrow::Cow::Borrowed(_))
        ));
    }

    /// The three fields come back as packed, at their ends and between.
    #[test]
    fn ac_token_fields_do_not_touch() {
        for local in [0, 1, 35, 255, 256, 510, 511] {
            for place in [0u8, 1, 7, 8, 23, 31] {
                for value in [0, 1, 63, 2046, 65_535, (1 << 18) - 1] {
                    let t = AcToken::new(local, place, value);
                    assert_eq!(
                        (t.local(), t.place(), t.value()),
                        (local, place as usize, value)
                    );
                }
            }
        }
    }
}
