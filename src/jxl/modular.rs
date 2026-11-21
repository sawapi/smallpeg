//! Modular coding: the tree, the weighted predictor, and the tokens of a
//! stream.

use super::bits::{Bits, pack_signed};
use super::entropy::Tokens;

pub(super) const PRED_ZERO: u32 = 0;
pub(super) const PRED_GRADIENT: u32 = 5;
pub(super) const PRED_WEIGHTED: u32 = 6;
pub(super) const PROP_STREAM: u32 = 1;
const PROP_WP: u32 = 15;

/// A meta-adaptive tree as built: a property threshold splits the values
/// above it to the left.
pub(super) enum Node {
    Split {
        prop: u32,
        val: i32,
        left: Box<Node>,
        right: Box<Node>,
    },
    Leaf {
        pred: u32,
    },
}

pub(super) fn split(prop: u32, val: i32, left: Node, right: Node) -> Node {
    Node::Split {
        prop,
        val,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// The tree in coded (breadth-first) order; leaves number their contexts
/// in that order.
enum Flat {
    Split { prop: u32, val: i32, left: usize },
    Leaf { pred: u32, ctx: u32 },
}

pub(super) struct Tree {
    nodes: Vec<Flat>,
    pub(super) leaves: usize,
    wp: bool,
}

impl Tree {
    pub(super) fn new(root: Node) -> Tree {
        let mut nodes = Vec::new();
        let mut queue = std::collections::VecDeque::from([root]);
        let mut leaves = 0;
        let mut wp = false;
        while let Some(n) = queue.pop_front() {
            match n {
                Node::Split {
                    prop,
                    val,
                    left,
                    right,
                } => {
                    wp |= prop == PROP_WP;
                    let left_at = nodes.len() + queue.len() + 1;
                    nodes.push(Flat::Split {
                        prop,
                        val,
                        left: left_at,
                    });
                    queue.push_back(*left);
                    queue.push_back(*right);
                }
                Node::Leaf { pred } => {
                    wp |= pred == PRED_WEIGHTED;
                    nodes.push(Flat::Leaf { pred, ctx: leaves });
                    leaves += 1;
                }
            }
        }
        Tree {
            nodes,
            leaves: leaves as usize,
            wp,
        }
    }

    /// The tree's own symbols, in its six contexts.
    pub(super) fn tokens(&self) -> Tokens {
        let mut t = Tokens::new();
        for n in &self.nodes {
            match *n {
                Flat::Split { prop, val, .. } => {
                    t.push((1, prop + 1));
                    t.push((0, pack_signed(val)));
                }
                Flat::Leaf { pred, .. } => {
                    t.push((1, 0));
                    t.push((2, pred));
                    t.push((3, 0)); // offset
                    t.push((4, 0)); // multiplier: 1
                    t.push((5, 0));
                }
            }
        }
        t
    }

    fn leaf(&self, props: &[i32; 16]) -> (u32, u32) {
        let mut i = 0;
        loop {
            match self.nodes[i] {
                Flat::Split { prop, val, left } => {
                    i = if props[prop as usize] > val {
                        left
                    } else {
                        left + 1
                    };
                }
                Flat::Leaf { pred, ctx } => return (pred, ctx),
            }
        }
    }
}

/// The weighted predictor with default parameters (JPEG XL's "self-
/// correcting" predictor): four sub-predictors weighted by their recent
/// errors around the pixel.
struct Wp {
    xsize: usize,
    pred_err: [Vec<u32>; 4],
    err: Vec<i32>,
    sub: [i64; 4],
    pub(super) pred: i64,
}

const WP_DIV: [u32; 64] = [
    16777216, 8388608, 5592405, 4194304, 3355443, 2796202, 2396745, 2097152, 1864135, 1677721,
    1525201, 1398101, 1290555, 1198372, 1118481, 1048576, 986895, 932067, 883011, 838860, 798915,
    762600, 729444, 699050, 671088, 645277, 621378, 599186, 578524, 559240, 541200, 524288, 508400,
    493447, 479349, 466033, 453438, 441505, 430185, 419430, 409200, 399457, 390167, 381300, 372827,
    364722, 356962, 349525, 342392, 335544, 328965, 322638, 316551, 310689, 305040, 299593, 294337,
    289262, 284359, 279620, 275036, 270600, 266305, 262144,
];

impl Wp {
    pub(super) fn new(xsize: usize) -> Self {
        let n = (xsize + 2) * 2;
        Wp {
            xsize,
            pred_err: std::array::from_fn(|_| vec![0; n]),
            err: vec![0; n],
            sub: [0; 4],
            pred: 0,
        }
    }

    fn rows(&self, y: usize) -> (usize, usize) {
        if y & 1 == 1 {
            (0, self.xsize + 2)
        } else {
            (self.xsize + 2, 0)
        }
    }

    /// Prediction and the tree property (largest recent error nearby).
    /// The default parameters give the two neighbors further out no
    /// weight, so they are not taken.
    fn predict(&mut self, x: usize, y: usize, n: i64, w: i64, ne: i64) -> (i64, i32) {
        const WEIGHTS: [u32; 4] = [0xd, 0xc, 0xc, 0xc];
        let (cur, prev) = self.rows(y);
        let pn = prev + x;
        let pne = if x + 1 < self.xsize { pn + 1 } else { pn };
        let pnw = if x > 0 { pn - 1 } else { pn };
        let mut wts = [0u32; 4];
        for i in 0..4 {
            let e = self.pred_err[i][pn]
                .wrapping_add(self.pred_err[i][pne])
                .wrapping_add(self.pred_err[i][pnw]) as u64;
            let shift = (63 - (e + 1).leading_zeros() as i32 - 5).max(0);
            wts[i] = 4 + ((WEIGHTS[i] * WP_DIV[(e >> shift) as usize]) >> shift);
        }
        let (n, w, ne) = (n << 3, w << 3, ne << 3);
        let te_w = if x == 0 {
            0
        } else {
            self.err[cur + x - 1] as i64
        };
        let te_n = self.err[pn] as i64;
        let te_nw = self.err[pnw] as i64;
        let te_ne = self.err[pne] as i64;
        let sum_wn = te_n + te_w;
        let mut p = te_w;
        for t in [te_n, te_nw, te_ne] {
            if t.abs() > p.abs() {
                p = t;
            }
        }
        self.sub = [
            w + ne - n,
            n - (((sum_wn + te_ne) * 16) >> 5),
            w - (((sum_wn + te_nw) * 10) >> 5),
            n - ((te_nw * 7 + te_n * 7 + te_ne * 7) >> 5),
        ];
        let log = 31 - wts.iter().sum::<u32>().leading_zeros();
        for v in wts.iter_mut() {
            *v >>= log - 4;
        }
        let ws: u32 = wts.iter().sum();
        let mut sum = (ws >> 1) as i64 - 1;
        for i in 0..4 {
            sum += self.sub[i] * wts[i] as i64;
        }
        let mut pred = (sum * WP_DIV[ws as usize - 1] as i64) >> 24;
        if ((te_n ^ te_w) | (te_n ^ te_nw)) <= 0 {
            pred = pred.clamp(w.min(ne).min(n), w.max(ne).max(n));
        }
        self.pred = pred;
        ((pred + 3) >> 3, p as i32)
    }

    fn update(&mut self, v: i64, x: usize, y: usize) {
        let (cur, prev) = self.rows(y);
        let v = v << 3;
        self.err[cur + x] = (self.pred - v) as i32;
        for i in 0..4 {
            let e = (((self.sub[i] - v).abs() + 3) >> 3) as u32;
            self.pred_err[i][cur + x] = e;
            self.pred_err[i][prev + x + 1] = self.pred_err[i][prev + x + 1].wrapping_add(e);
        }
    }
}

/// Tokens of one modular stream: its channels (values, width, height)
/// coded through `tree`.
pub(super) fn stream_tokens(
    tree: &Tree,
    stream: u32,
    chans: &[(&[i32], usize, usize)],
    out: &mut Tokens,
    mut trace: Option<&mut Vec<(i32, i32)>>,
) {
    let mut props = [0i32; 16];
    props[PROP_STREAM as usize] = stream as i32;
    for (i, &(v, w, h)) in chans.iter().enumerate() {
        props[0] = i as i32;
        let mut wp = tree.wp.then(|| Wp::new(w));
        for y in 0..h {
            for x in 0..w {
                let at = |xx: usize, yy: usize| v[yy * w + xx] as i64;
                let left = if x > 0 {
                    at(x - 1, y)
                } else if y > 0 {
                    at(x, y - 1)
                } else {
                    0
                };
                let top = if y > 0 { at(x, y - 1) } else { left };
                let topleft = if x > 0 && y > 0 {
                    at(x - 1, y - 1)
                } else {
                    left
                };
                let topright = if x + 1 < w && y > 0 {
                    at(x + 1, y - 1)
                } else {
                    top
                };
                let mut wp_guess = 0;
                if let Some(wp) = wp.as_mut() {
                    let (g, p) = wp.predict(x, y, top, left, topright);
                    wp_guess = g;
                    props[PROP_WP as usize] = p;
                }
                if let Some(t) = trace.as_mut() {
                    t.push((props[0], props[PROP_WP as usize]));
                }
                let (pred, ctx) = tree.leaf(&props);
                let guess = match pred {
                    PRED_ZERO => 0,
                    PRED_GRADIENT => {
                        let (lo, hi) = (left.min(top), left.max(top));
                        if topleft < lo {
                            hi
                        } else if topleft > hi {
                            lo
                        } else {
                            left + top - topleft
                        }
                    }
                    _ => wp_guess,
                };
                let val = v[y * w + x] as i64;
                out.push((ctx, pack_signed((val - guess) as i32)));
                if let Some(wp) = wp.as_mut() {
                    wp.update(val, x, y);
                }
            }
        }
    }
}

/// Estimated bits to code `h` with its own histogram, the histogram
/// itself included.
pub(super) fn hist_cost(h: &[u32; 128]) -> f64 {
    let n: u32 = h.iter().sum();
    if n == 0 {
        return 0.0;
    }
    let mut bits = 24.0;
    for &c in h.iter().filter(|&&c| c > 0) {
        bits += c as f64 * (n as f64 / c as f64).log2() + 6.0;
    }
    bits
}

/// The DC part of the tree, learned from the DC itself: `samples` are
/// (channel, weighted predictor error property, residual token). Each node
/// takes the split, on the channel or the property, that most reduces
/// the estimated cost, if it pays for the context it adds.
pub(super) fn dc_tree(samples: &mut [(i32, i32, u8)], depth: u32) -> Node {
    let leaf = Node::Leaf {
        pred: PRED_WEIGHTED,
    };
    if depth == 0 || samples.len() < 64 {
        return leaf;
    }
    let mut all = [0u32; 128];
    for s in samples.iter() {
        all[s.2 as usize] += 1;
    }
    let mut best = (hist_cost(&all), 0, 0); // (cost, prop, value)
    for prop in [0, PROP_WP] {
        let key = |s: &(i32, i32, u8)| if prop == 0 { s.0 } else { s.1 };
        samples.sort_unstable_by_key(key);
        // Try up to 64 cuts, spread evenly through the sorted samples.
        let step = (samples.len() / 64).max(1);
        let mut low = [0u32; 128];
        let mut next = step;
        for i in 0..samples.len() - 1 {
            low[samples[i].2 as usize] += 1;
            let (k, k1) = (key(&samples[i]), key(&samples[i + 1]));
            if i + 1 < next || k == k1 {
                continue;
            }
            next = i + 1 + step;
            let mut high = all;
            for (h, l) in high.iter_mut().zip(&low) {
                *h -= l;
            }
            let cost = hist_cost(&low) + hist_cost(&high);
            if cost < best.0 {
                best = (cost, prop, k);
            }
        }
    }
    let (_, prop, val) = best;
    if prop == 0 && best.0 >= hist_cost(&all) {
        return leaf;
    }
    let key = |s: &(i32, i32, u8)| if prop == 0 { s.0 } else { s.1 };
    samples.sort_unstable_by_key(key);
    let at = samples.partition_point(|s| key(s) <= val);
    let (low, high) = samples.split_at_mut(at);
    split(prop, val, dc_tree(high, depth - 1), dc_tree(low, depth - 1))
}

/// A modular sub-bitstream header using the global tree, no transforms.
pub(super) fn modular_header(w: &mut Bits) {
    w.bit(true); // global tree
    w.bit(true); // default weighted predictor parameters
    w.put(2, 0); // no transforms
}
