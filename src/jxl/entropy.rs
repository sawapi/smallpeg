//! Entropy coding: clustering contexts, prefix and ANS codes.

use super::bits::{Bits, ceil_log2};

/// A value split into a prefix-coded token and raw bits: the hybrid
/// integer configuration with split exponent 4, two mantissa bits in the
/// token and none from the bottom.
pub(super) fn hybrid(v: u32) -> (usize, u32, u32) {
    if v < 16 {
        return (v as usize, 0, 0);
    }
    let n = 31 - v.leading_zeros();
    let m = (v >> (n - 2)) & 3;
    let nb = n - 2;
    ((16 + ((n - 4) << 2) + m) as usize, nb, v & ((1 << nb) - 1))
}

/// Symbols to code: (context, value).
pub(super) type Tokens = Vec<(u32, u32)>;

/// Entropy codes for a set of contexts, and the map from context to code.
pub(super) struct Code {
    ctx_map: Vec<u8>,
    codes: Codes,
}

enum Codes {
    Prefix {
        lens: Vec<Vec<u8>>,
        codes: Vec<Vec<u16>>,
    },
    /// Normalized frequencies (summing to 4096) and, per symbol and
    /// offset, the state slot the decoder maps back to them.
    Ans {
        freq: Vec<Vec<u16>>,
        slots: Vec<Vec<Vec<u16>>>,
    },
}

impl Code {
    /// Writes one stream of symbols (one decoder reader's worth).
    /// Codes `tokens`, (context, value) each, in order.
    pub(super) fn put_all<I>(&self, w: &mut Bits, tokens: I)
    where
        I: DoubleEndedIterator<Item = (u32, u32)>,
    {
        match &self.codes {
            Codes::Prefix { lens, codes } => {
                for (c, v) in tokens {
                    let cl = self.ctx_map[c as usize] as usize;
                    let (t, nb, bits) = hybrid(v);
                    w.put(lens[cl][t] as u32, codes[cl][t] as u64);
                    w.put(nb, bits as u64);
                }
            }
            Codes::Ans { freq, slots } => {
                // ANS codes backwards; the decoder then reads the final
                // state first and each symbol's refill before its raw bits.
                let mut state: u32 = 0x13_0000;
                let mut out = Vec::with_capacity(tokens.size_hint().0);
                for (c, v) in tokens.rev() {
                    let cl = self.ctx_map[c as usize] as usize;
                    let (t, nb, bits) = hybrid(v);
                    let f = freq[cl][t] as u32;
                    let mut refill = None;
                    if (state >> 20) >= f {
                        refill = Some(state & 0xFFFF);
                        state >>= 16;
                    }
                    state = ((state / f) << 12) + slots[cl][t][(state % f) as usize] as u32;
                    out.push((refill, nb, bits));
                }
                w.put(32, state as u64);
                for &(refill, nb, bits) in out.iter().rev() {
                    if let Some(r) = refill {
                        w.put(16, r as u64);
                    }
                    w.put(nb, bits as u64);
                }
            }
        }
    }
}

/// Bits to code `h` with an ideal code for it.
fn entropy(h: &[u32]) -> f64 {
    let total: u64 = h.iter().map(|&c| c as u64).sum();
    if total == 0 {
        return 0.0;
    }
    let t = total as f64;
    h.iter()
        .filter(|&&c| c > 0)
        .map(|&c| c as f64 * (t / c as f64).log2())
        .sum()
}

/// Estimated cost of a histogram, its code description included.
fn cost(h: &[u32]) -> f64 {
    entropy(h) + 4.0 * h.iter().filter(|&&c| c > 0).count() as f64 + 16.0
}

fn add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| a.get(i).copied().unwrap_or(0) + b.get(i).copied().unwrap_or(0))
        .collect()
}

/// Groups contexts with similar statistics: returns the cluster of each
/// context and the summed histogram of each cluster.
fn cluster(hists: &[Vec<u32>]) -> (Vec<u8>, Vec<Vec<u32>>) {
    const MAX_CLUSTERS: usize = 64;
    let used: Vec<usize> = (0..hists.len())
        .filter(|&i| hists[i].iter().any(|&c| c > 0))
        .collect();
    let mut map = vec![0u8; hists.len()];
    if used.is_empty() {
        return (map, vec![vec![0]]);
    }
    // Seeds, farthest first: each next seed is the histogram that would
    // cost the most to merge into the nearest seed so far.
    let merge_cost = |h: &[u32], c: &[u32]| cost(&add(h, c)) - cost(c) - cost(h);
    let mut seeds: Vec<usize> = vec![
        *used
            .iter()
            .max_by_key(|&&i| hists[i].iter().map(|&c| c as u64).sum::<u64>())
            .unwrap(),
    ];
    let mut dist: Vec<f64> = used
        .iter()
        .map(|&i| merge_cost(&hists[i], &hists[seeds[0]]))
        .collect();
    while seeds.len() < MAX_CLUSTERS.min(used.len()) {
        let (k, &d) = dist
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        // Merging costs about nothing: a new code would not pay for itself.
        if d < 64.0 {
            break;
        }
        let s = used[k];
        seeds.push(s);
        for (j, &i) in used.iter().enumerate() {
            dist[j] = dist[j].min(merge_cost(&hists[i], &hists[s]));
        }
    }
    // Assign each context to its cheapest cluster, then refine once.
    let mut centers: Vec<Vec<u32>> = seeds.iter().map(|&s| hists[s].clone()).collect();
    for _ in 0..2 {
        let mut next: Vec<Vec<u32>> = vec![Vec::new(); centers.len()];
        for &i in &used {
            let best = (0..centers.len())
                .min_by(|&a, &b| {
                    merge_cost(&hists[i], &centers[a])
                        .partial_cmp(&merge_cost(&hists[i], &centers[b]))
                        .unwrap()
                })
                .unwrap();
            map[i] = best as u8;
            next[best] = add(&next[best], &hists[i]);
        }
        centers = next;
    }
    // Drop empty clusters and renumber.
    let mut renum = vec![u8::MAX; centers.len()];
    let mut out = Vec::new();
    for &i in &used {
        let c = map[i] as usize;
        if renum[c] == u8::MAX {
            renum[c] = out.len() as u8;
            out.push(centers[c].clone());
        }
    }
    let mut prev = 0u8;
    for i in 0..hists.len() {
        if hists[i].iter().any(|&c| c > 0) {
            map[i] = renum[map[i] as usize];
            prev = map[i];
        } else {
            // Unused contexts copy their neighbor, which codes cheaply.
            map[i] = prev;
        }
    }
    (map, out)
}

/// Huffman code lengths for `freq`, at most `limit` bits.
fn huffman_lengths(freq: &[u32], limit: u8) -> Vec<u8> {
    let mut f: Vec<u64> = freq.iter().map(|&c| c as u64).collect();
    loop {
        let lens = huffman_unlimited(&f);
        if lens.iter().all(|&l| l <= limit) {
            return lens;
        }
        // Flatten the distribution and try again.
        for c in f.iter_mut().filter(|c| **c > 0) {
            *c = (*c >> 1).max(1) + 1;
        }
    }
}

fn huffman_unlimited(f: &[u64]) -> Vec<u8> {
    let mut lens = vec![0u8; f.len()];
    // Nodes: (weight, children or leaf symbol).
    // (weight, children, leaf symbol)
    type HuffNode = (u64, Option<(usize, usize)>, usize);
    let mut nodes: Vec<HuffNode> = Vec::new();
    let mut heap = std::collections::BinaryHeap::new();
    for (s, &w) in f.iter().enumerate() {
        if w > 0 {
            heap.push(std::cmp::Reverse((w, nodes.len())));
            nodes.push((w, None, s));
        }
    }
    if nodes.len() == 1 {
        lens[nodes[0].2] = 1;
        return lens;
    }
    while heap.len() > 1 {
        let std::cmp::Reverse((wa, a)) = heap.pop().unwrap();
        let std::cmp::Reverse((wb, b)) = heap.pop().unwrap();
        heap.push(std::cmp::Reverse((wa + wb, nodes.len())));
        nodes.push((wa + wb, Some((a, b)), 0));
    }
    let mut stack = vec![(nodes.len() - 1, 0u8)];
    while let Some((i, d)) = stack.pop() {
        match nodes[i].1 {
            Some((a, b)) => {
                stack.push((a, d + 1));
                stack.push((b, d + 1));
            }
            None => lens[nodes[i].2] = d.max(1),
        }
    }
    lens
}

/// Canonical codes for `lens`, bit-reversed for writing low bit first.
fn canonical(lens: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 17];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 17];
    let mut code = 0u16;
    for l in 1..17 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            c.reverse_bits() >> (16 - l)
        })
        .collect()
}

/// Writes one prefix code. `lens` covers the whole alphabet; returns the
/// lengths actually in effect (a lone symbol takes no bits).
fn write_prefix_code(w: &mut Bits, lens: &[u8]) -> Vec<u8> {
    let alphabet = lens.len();
    if alphabet == 1 {
        return vec![0];
    }
    let used: Vec<usize> = (0..alphabet).filter(|&i| lens[i] > 0).collect();
    if used.len() == 1 {
        w.put(2, 1); // simple code
        w.put(2, 0); // one symbol
        w.put(ceil_log2(alphabet), used[0] as u64);
        return vec![0; alphabet];
    }
    w.put(2, 0); // no skipped code length codes
    let mut cl_freq = [0u32; 18];
    for &l in lens {
        cl_freq[l as usize] += 1;
    }
    let mut cl_lens = huffman_lengths(&cl_freq, 5);
    let distinct = cl_lens.iter().filter(|&&l| l > 0).count();
    if distinct == 1 {
        // A single code length value is coded with no bits.
        for l in cl_lens.iter_mut().filter(|l| **l > 0) {
            *l = 1;
        }
    }
    const ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    // The static code for code length code lengths 0..=5: (bits, value).
    const STATIC: [(u32, u64); 6] = [(2, 0), (4, 7), (3, 3), (2, 2), (2, 1), (4, 15)];
    let mut space = 32i32;
    for &s in &ORDER {
        let l = cl_lens[s] as usize;
        let (nb, v) = STATIC[l];
        w.put(nb, v);
        if l > 0 {
            space -= 32 >> l;
        }
        if distinct > 1 && space == 0 {
            break;
        }
    }
    let cl_codes = canonical(&cl_lens);
    for &l in lens {
        let cl = if distinct == 1 {
            0
        } else {
            cl_lens[l as usize]
        };
        w.put(cl as u32, cl_codes[l as usize] as u64);
    }
    lens.to_vec()
}

fn write_varint16(w: &mut Bits, v: u32) {
    if v == 0 {
        w.bit(false);
    } else {
        let n = 31 - v.leading_zeros();
        w.bit(true);
        w.put(4, n as u64);
        w.put(n, (v - (1 << n)) as u64);
    }
}

/// Writes a context map for `map` (more than one context).
pub(super) fn write_context_map(w: &mut Bits, map: &[u8]) {
    let n = *map.iter().max().unwrap() as usize + 1;
    let mut best: Option<Bits> = None;
    if n <= 8 {
        let mut s = Bits::default();
        s.bit(true);
        let bits = ceil_log2(n);
        s.put(2, bits as u64);
        for &m in map {
            s.put(bits, m as u64);
        }
        best = Some(s);
    }
    for mtf in [false, true] {
        let mut s = Bits::default();
        s.bit(false);
        s.bit(mtf);
        let vals: Vec<u32> = if mtf {
            let mut order: Vec<u8> = (0..=255).collect();
            map.iter()
                .map(|&m| {
                    let i = order.iter().position(|&o| o == m).unwrap();
                    order.remove(i);
                    order.insert(0, m);
                    i as u32
                })
                .collect()
        } else {
            map.iter().map(|&m| m as u32).collect()
        };
        let tokens: Tokens = vals.iter().map(|&v| (0, v)).collect();
        let code = write_histograms(&mut s, 1, tokens.iter().copied());
        code.put_all(&mut s, tokens.iter().copied());
        if best.as_ref().is_none_or(|b| s.len() < b.len()) {
            best = Some(s);
        }
    }
    w.append(&best.unwrap());
}

/// Clusters the contexts of `tokens`, writes the histograms (context map,
/// integer configurations and codes, prefix or ANS, whichever is smaller)
/// and returns the codes.
pub(super) fn write_histograms(
    w: &mut Bits,
    num_ctx: usize,
    tokens: impl Iterator<Item = (u32, u32)>,
) -> Code {
    let mut hists: Vec<Vec<u32>> = vec![Vec::new(); num_ctx];
    for (c, v) in tokens {
        let t = hybrid(v).0;
        let h = &mut hists[c as usize];
        if h.len() <= t {
            h.resize(t + 1, 0);
        }
        h[t] += 1;
    }
    let (ctx_map, clusters) = if num_ctx == 1 {
        (vec![0], vec![hists[0].clone()])
    } else {
        cluster(&hists)
    };
    w.bit(false); // no LZ77
    if num_ctx > 1 {
        write_context_map(w, &ctx_map);
    }
    // The alphabet of each cluster ends at its last symbol used.
    let clusters: Vec<Vec<u32>> = clusters
        .into_iter()
        .map(|mut h| {
            let size = h.iter().rposition(|&c| c > 0).map_or(1, |i| i + 1);
            h.resize(size, 0);
            h
        })
        .collect();

    let (pw, prefix, pbits) = prefix_codes(&clusters);
    let max_alpha = clusters.iter().map(|h| h.len()).max().unwrap();
    let log_alpha = ceil_log2(max_alpha).max(5);
    if log_alpha <= 8 {
        let (aw, ans, abits) = ans_codes(&clusters, log_alpha);
        if aw.len() as f64 + abits + 32.0 < pw.len() as f64 + pbits {
            w.append(&aw);
            return Code {
                ctx_map,
                codes: ans,
            };
        }
    }
    w.append(&pw);
    Code {
        ctx_map,
        codes: prefix,
    }
}

/// Prefix codes for `clusters`: the written description, the codes and
/// the bits the symbols will take (raw bits excluded).
fn prefix_codes(clusters: &[Vec<u32>]) -> (Bits, Codes, f64) {
    let mut w = Bits::default();
    w.bit(true); // prefix codes
    for _ in clusters {
        w.put(4, 4); // split exponent
        w.put(3, 2); // mantissa bits in the token
        w.put(2, 0); // low bits in the token
    }
    for h in clusters {
        write_varint16(&mut w, h.len() as u32 - 1);
    }
    let mut lens = Vec::new();
    let mut codes = Vec::new();
    let mut bits = 0.0;
    for h in clusters {
        let l = huffman_lengths(h, 15);
        let eff = write_prefix_code(&mut w, &l);
        bits += h
            .iter()
            .zip(&eff)
            .map(|(&c, &l)| c as f64 * l as f64)
            .sum::<f64>();
        codes.push(if eff.iter().all(|&x| x == 0) {
            vec![0; eff.len()]
        } else {
            canonical(&eff)
        });
        lens.push(eff);
    }
    (w, Codes::Prefix { lens, codes }, bits)
}

/// ANS codes for `clusters`, as [`prefix_codes`].
fn ans_codes(clusters: &[Vec<u32>], log_alpha: u32) -> (Bits, Codes, f64) {
    let mut w = Bits::default();
    w.bit(false); // ANS
    w.put(2, (log_alpha - 5) as u64);
    let se_bits = ceil_log2(log_alpha as usize + 1);
    for _ in clusters {
        w.put(se_bits, 4); // split exponent
        w.put(3, 2); // mantissa bits in the token: ceil_log2(4 + 1) bits
        w.put(2, 0); // low bits: ceil_log2(4 - 2 + 1) bits
    }
    let mut freq = Vec::new();
    let mut slots = Vec::new();
    let mut bits = 0.0;
    for h in clusters {
        let f = write_ans_histogram(&mut w, h, log_alpha);
        bits += h
            .iter()
            .zip(&f)
            .filter(|(c, _)| **c > 0)
            .map(|(&c, &f)| c as f64 * (4096.0 / f as f64).log2())
            .sum::<f64>();
        slots.push(ans_slots(&f, log_alpha));
        freq.push(f);
    }
    (w, Codes::Ans { freq, slots }, bits)
}

fn write_u8(w: &mut Bits, v: u32) {
    if v == 0 {
        w.bit(false);
    } else {
        let n = 31 - v.leading_zeros();
        w.bit(true);
        w.put(3, n as u64);
        w.put(n, (v - (1 << n)) as u64);
    }
}

/// Writes the ANS distribution for counts `h` and returns the normalized
/// frequencies the decoder will use (the full table of `1 << log_alpha`).
pub(super) fn write_ans_histogram(w: &mut Bits, h: &[u32], log_alpha: u32) -> Vec<u16> {
    let table = 1usize << log_alpha;
    let mut f = vec![0u16; table];
    let used: Vec<usize> = (0..h.len()).filter(|&i| h[i] > 0).collect();
    if used.len() <= 1 {
        let s = used.first().copied().unwrap_or(0);
        w.bit(true);
        w.bit(false);
        write_u8(w, s as u32);
        f[s] = 4096;
        return f;
    }
    let total: u64 = h.iter().map(|&c| c as u64).sum();
    if used.len() == 2 {
        let (a, b) = (used[0], used[1]);
        let p = ((h[a] as u64 * 4096 + total / 2) / total).clamp(1, 4095) as u16;
        w.bit(true);
        w.bit(true);
        write_u8(w, a as u32);
        write_u8(w, b as u32);
        w.put(12, p as u64);
        f[a] = p;
        f[b] = 4096 - p;
        return f;
    }
    // The general form: each frequency's bit length, then its leading
    // bits at a precision set by `shift`. Pick the cheapest shift.
    let mut best: Option<(f64, Bits, Vec<u16>)> = None;
    for shift in 0..=13u32 {
        let Some((freqs, codes, omit)) = normalize_ans(h, shift) else {
            continue;
        };
        let mut b = Bits::default();
        b.bit(false);
        b.bit(false);
        let len = match shift {
            0 => 0,
            1..=2 => 1,
            3..=6 => 2,
            _ => 3,
        };
        for _ in 0..len {
            b.bit(true);
        }
        if len < 3 {
            b.bit(false);
        }
        b.put(len, (shift + 1 - (1 << len)) as u64);
        write_u8(&mut b, h.len() as u32 - 3);
        for &c in &codes {
            let (nb, v) = LOGCOUNT_CODE[c as usize];
            b.put(nb, v);
        }
        for i in 0..h.len() {
            if codes[i] > 1 && i != omit {
                let zeros = codes[i] as u32 - 1;
                let bc = ans_precision(shift, zeros);
                b.put(
                    bc,
                    ((freqs[i] as u32 - (1 << zeros)) >> (zeros - bc)) as u64,
                );
            }
        }
        let data: f64 = (0..h.len())
            .filter(|&i| h[i] > 0)
            .map(|i| h[i] as f64 * (4096.0 / freqs[i] as f64).log2())
            .sum();
        let cost = b.len() as f64 + data;
        if best.as_ref().is_none_or(|(c, _, _)| cost < *c) {
            best = Some((cost, b, freqs));
        }
    }
    let (_, b, freqs) = best.expect("shift 13 always fits");
    w.append(&b);
    f[..freqs.len()].copy_from_slice(&freqs);
    f
}

/// Static prefix code for the bit lengths of ANS frequencies (and 13, a
/// repeat marker, unused here): (bits, value).
const LOGCOUNT_CODE: [(u32, u64); 14] = [
    (5, 17),
    (4, 11),
    (4, 15),
    (4, 3),
    (4, 9),
    (4, 7),
    (3, 4),
    (3, 2),
    (3, 5),
    (3, 6),
    (3, 0),
    (6, 33),
    (7, 1),
    (7, 65),
];

/// Bits stored below the leading one of a frequency `2^zeros..`.
fn ans_precision(shift: u32, zeros: u32) -> u32 {
    (shift as i32 - ((12 - zeros as i32) >> 1)).clamp(0, zeros as i32) as u32
}

/// Frequencies for `h` that the general form can carry at `shift`, with
/// their length codes and the symbol that takes the remainder.
fn normalize_ans(h: &[u32], shift: u32) -> Option<(Vec<u16>, Vec<u8>, usize)> {
    let total: u64 = h.iter().map(|&c| c as u64).sum();
    let mut f: Vec<u32> = h
        .iter()
        .map(|&c| {
            if c == 0 {
                0
            } else {
                ((c as u64 * 4096 + total / 2) / total).max(1) as u32
            }
        })
        .collect();
    for v in f.iter_mut().filter(|v| **v > 1) {
        let zeros = 31 - v.leading_zeros();
        let drop = zeros - ans_precision(shift, zeros);
        *v = ((*v + (1 << drop >> 1)) >> drop) << drop;
    }
    // The step a frequency of `v` can move by at this shift.
    let step = |v: u32| {
        let zeros = 31 - v.leading_zeros();
        1u32 << (zeros - ans_precision(shift, zeros))
    };
    // A symbol with nearly all the mass may round up to the whole table,
    // which the length codes cannot carry.
    for v in f.iter_mut().filter(|v| **v >= 4096) {
        *v = 4096 - step(2048);
    }
    let code = |v: u32| {
        if v == 0 {
            0
        } else {
            (32 - v.leading_zeros()) as u8
        }
    };
    loop {
        let codes: Vec<u8> = f.iter().map(|&v| code(v)).collect();
        let max = *codes.iter().max().unwrap();
        let omit = codes.iter().position(|&c| c == max).unwrap();
        let rest: u32 = f
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != omit)
            .map(|(_, &v)| v)
            .sum();
        if rest < 4096 {
            f[omit] = 4096 - rest;
            return Some((f.iter().map(|&v| v as u16).collect(), codes, omit));
        }
        // Many rare symbols rounded up to 1 overfill the table: take from
        // the largest of the others, by a step that stays representable.
        let i = (0..f.len())
            .filter(|&i| i != omit && f[i] > 1)
            .max_by_key(|&i| f[i])?;
        f[i] -= step(f[i]);
    }
}

/// For each symbol and offset below its frequency, the 12-bit state slot
/// that the decoder's alias table maps to it.
fn ans_slots(f: &[u16], log_alpha: u32) -> Vec<Vec<u16>> {
    let table = f.len();
    let log_bucket = 12 - log_alpha;
    let bucket = 1u32 << log_bucket;
    // (symbol, offset) for every slot, as the decoder derives it.
    let mut map = vec![(0usize, 0u32); 4096];
    if let Some(s) = f.iter().position(|&v| v == 4096) {
        for (idx, m) in map.iter_mut().enumerate() {
            *m = (s, idx as u32);
        }
    } else {
        let alphabet = f.iter().rposition(|&v| v > 0).map_or(0, |i| i + 1);
        let mut cutoff: Vec<u32> = f.iter().map(|&v| v as u32).collect();
        let mut alias_sym: Vec<usize> = (0..table)
            .map(|i| if i < alphabet { i } else { 0 })
            .collect();
        let mut alias_off = vec![0u32; table];
        let mut under: Vec<usize> = Vec::new();
        let mut over: Vec<usize> = Vec::new();
        for i in 0..table {
            if cutoff[i] < bucket {
                under.push(i);
            } else if cutoff[i] > bucket {
                over.push(i);
            }
        }
        while let (Some(&o), Some(&u)) = (over.last(), under.last()) {
            over.pop();
            under.pop();
            let by = bucket - cutoff[u];
            cutoff[o] -= by;
            alias_sym[u] = o;
            alias_off[u] = cutoff[o];
            if cutoff[o] < bucket {
                under.push(o);
            } else if cutoff[o] > bucket {
                over.push(o);
            }
        }
        for idx in 0..4096u32 {
            let i = (idx >> log_bucket) as usize;
            let pos = idx & (bucket - 1);
            map[idx as usize] = if cutoff[i] == bucket || pos >= cutoff[i] {
                if cutoff[i] == bucket {
                    (i, pos)
                } else {
                    let off = (alias_off[i] as u16).wrapping_sub(cutoff[i] as u16) as u32;
                    (alias_sym[i], off + pos)
                }
            } else {
                (i, pos)
            };
        }
    }
    let mut slots: Vec<Vec<u16>> = f.iter().map(|&v| vec![u16::MAX; v as usize]).collect();
    for (idx, &(s, off)) in map.iter().enumerate() {
        slots[s][off as usize] = idx as u16;
    }
    debug_assert!(slots.iter().all(|v| v.iter().all(|&x| x != u16::MAX)));
    slots
}
