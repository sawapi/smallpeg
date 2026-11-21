//! JPEG to JPEG XL recompression. Experimental: behind the `jxl`
//! feature, and outside the crate's compatibility promise until it
//! settles; the functions and their output may change in any release.
//!
//! The JPEG's quantized DCT coefficients are stored as they are in a
//! JPEG XL VarDCT frame (8x8 DCT blocks, the JPEG's own quantization
//! tables, YCbCr and the JPEG's chroma subsampling), so the image decodes
//! to what the JPEG holds, with no generation loss. Only the entropy
//! coding changes. Reconstruction data (`recon`) lets a decoder write the
//! JPEG file itself back.
//!
//! Written from the JPEG XL format (ISO/IEC 18181-1).

use crate::optimize::{Limits, Parsed, parse_and_decode};
use crate::quant::ZIGZAG;

mod recon;

/// Recompress a baseline or progressive YCbCr JPEG as JPEG XL without
/// changing its coefficients. `None` for files this cannot carry over
/// (grayscale, CMYK, RGB-coded JPEGs, sampling factors other than 1 and 2,
/// a zero in a quantization table).
///
/// The result also holds what a JPEG XL decoder needs to write the
/// original file back, byte for byte, whenever the format can describe
/// the file (a few it cannot, such as one that changes its restart
/// interval between scans; the image is carried over all the same).
/// That adds a few hundred bytes; [`JxlOptions::reconstruction`] turns it
/// off.
///
/// Experimental (the `jxl` feature): this function and its output may
/// change in any release.
pub fn jpeg_to_jxl(data: &[u8]) -> Option<Vec<u8>> {
    jpeg_to_jxl_with(data, &JxlOptions::default())
}

/// Options for [`jpeg_to_jxl_with`]. Experimental like the functions.
///
/// ```
/// // Leave out what rebuilds the JPEG file, for the smallest output:
/// let options = smallpeg::JxlOptions::default().reconstruction(false);
/// # assert!(!options.reconstruction);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct JxlOptions {
    /// Limits on the JPEG that is read.
    pub limits: Limits,
    /// Include what a decoder needs to write the original JPEG file back
    /// byte for byte (a few hundred bytes), when the format can describe
    /// the file. Default true. Without it the image is the same, but the
    /// file it came from is gone for good.
    pub reconstruction: bool,
}

impl Default for JxlOptions {
    fn default() -> Self {
        JxlOptions {
            limits: Limits::default(),
            reconstruction: true,
        }
    }
}

impl JxlOptions {
    /// Set [`limits`](JxlOptions::limits).
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Set [`reconstruction`](JxlOptions::reconstruction).
    pub fn reconstruction(mut self, on: bool) -> Self {
        self.reconstruction = on;
        self
    }
}

/// [`jpeg_to_jxl`] with options. Experimental in the same way.
pub fn jpeg_to_jxl_with(data: &[u8], options: &JxlOptions) -> Option<Vec<u8>> {
    let p = parse_and_decode(data, false, &options.limits)?;
    let f = Frame::new(&p)?;
    let meta = Meta::read(data, &p);
    let codestream = f.encode(&meta);
    let jbrd = (options.reconstruction)
        .then(|| recon::data(data, &p, &meta))
        .flatten();
    Some(
        if meta.exif.is_none() && meta.xmp.is_none() && jbrd.is_none() {
            codestream
        } else {
            container(&codestream, &meta, jbrd.as_deref())
        },
    )
}

/// What carries over from the JPEG besides the image: the color profile
/// and orientation (in the codestream), Exif and XMP (in boxes). The
/// reconstruction data counts on finding them there as they are.
struct Meta {
    icc: Option<Vec<u8>>,
    orientation: u8,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
}

impl Meta {
    fn read(data: &[u8], p: &Parsed) -> Meta {
        // (sequence number, count, data)
        let mut icc_parts: Vec<(u8, u8, &[u8])> = Vec::new();
        let orientation = crate::optimize::jpeg_info(data).map_or(1, |i| i.orientation);
        // The Exif segment the orientation was read from when there are
        // several, so the two agree; otherwise the first.
        let mut exif: Option<Vec<u8>> = None;
        let mut exif_agrees = false;
        let mut xmp = None;
        for r in &p.meta {
            let marker = data[r.start + 1];
            let payload = &data[(r.start + 4).min(r.end)..r.end];
            if marker == 0xE2 && payload.len() > 14 && payload.starts_with(b"ICC_PROFILE\0") {
                icc_parts.push((payload[12], payload[13], &payload[14..]));
            } else if marker == 0xE1 && payload.starts_with(b"Exif\0\0") {
                let agrees = crate::meta::exif_orientation(payload) == Some(orientation);
                if exif.is_none() || (agrees && !exif_agrees) {
                    exif = Some(payload[6..].to_vec());
                    exif_agrees = agrees;
                }
            } else if marker == 0xE1
                && payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                && xmp.is_none()
            {
                xmp = Some(payload[29..].to_vec());
            }
        }
        icc_parts.sort_by_key(|&(seq, _, _)| seq);
        // Only a whole profile: chunks numbered 1 to n, each once, all
        // saying n, and a header whose size and signature fit. Viewers
        // ignore anything else, and so does the output (sRGB).
        let n = icc_parts.len();
        let whole = n > 0
            && icc_parts
                .iter()
                .enumerate()
                .all(|(i, &(seq, count, _))| seq as usize == i + 1 && count as usize == n);
        let icc = whole
            .then(|| -> Vec<u8> {
                icc_parts
                    .iter()
                    .flat_map(|&(_, _, d)| d.iter().copied())
                    .collect()
            })
            .filter(|icc| {
                icc.len() >= 132
                    && u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize == icc.len()
                    && &icc[36..40] == b"acsp"
            });
        Meta {
            icc,
            orientation,
            exif,
            xmp,
        }
    }
}

/// The codestream in the JPEG XL container, with the reconstruction data
/// and Exif and XMP boxes.
fn container(codestream: &[u8], meta: &Meta, jbrd: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(codestream.len() + 1024);
    let mut push_box = |kind: &[u8; 4], parts: &[&[u8]]| {
        let len: usize = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
        out.extend_from_slice(&(len as u32).to_be_bytes());
        out.extend_from_slice(kind);
        for p in parts {
            out.extend_from_slice(p);
        }
    };
    push_box(b"JXL ", &[&[0x0D, 0x0A, 0x87, 0x0A]]);
    push_box(b"ftyp", &[b"jxl ", &[0, 0, 0, 0], b"jxl "]);
    if let Some(j) = jbrd {
        push_box(b"jbrd", &[j]);
    }
    if let Some(e) = &meta.exif {
        push_box(b"Exif", &[&[0, 0, 0, 0], e]); // offset of the TIFF header: 0
    }
    if let Some(x) = &meta.xmp {
        push_box(b"xml ", &[x]);
    }
    push_box(b"jxlc", &[codestream]);
    out
}

/// The ICC profile as JPEG XL codes it: a byte stream (here the profile
/// unpredicted: its header relative to the expected one, the rest
/// copied), entropy coded with contexts from the previous two bytes.
fn write_icc(w: &mut Bits, icc: &[u8]) {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 128 {
            out.push((v as u8 & 127) | 128);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let size = icc.len();
    let mut commands = Vec::new();
    if size > 128 {
        varint(&mut commands, 0); // no tag list
        commands.push(1); // copy the rest
        varint(&mut commands, (size - 128) as u64);
    }
    let mut stream = Vec::with_capacity(size + 16);
    varint(&mut stream, size as u64);
    varint(&mut stream, commands.len() as u64);
    stream.extend_from_slice(&commands);
    let head = &icc[..size.min(128)];
    for (i, &b) in head.iter().enumerate() {
        stream.push(b.wrapping_sub(icc_header_prediction(i, size as u32, head)));
    }
    stream.extend_from_slice(&icc[size.min(128)..]);

    let class = |b: u8| -> (u32, u32) {
        let p1 = match b {
            b'a'..=b'z' | b'A'..=b'Z' => 0,
            b'0'..=b'9' | b'.' | b',' => 1,
            0..=1 => 2 + b as u32,
            2..=15 => 4,
            241..=254 => 5,
            255 => 6,
            _ => 7,
        };
        let p2 = match b {
            b'a'..=b'z' | b'A'..=b'Z' => 0,
            b'0'..=b'9' | b'.' | b',' => 1,
            0..=15 => 2,
            241..=255 => 3,
            _ => 4,
        };
        (p1, p2)
    };
    let mut tokens = Tokens::with_capacity(stream.len());
    for (i, &b) in stream.iter().enumerate() {
        let ctx = if i <= 128 {
            0
        } else {
            1 + class(stream[i - 1]).0 + 8 * class(stream[i - 2]).1
        };
        tokens.push((ctx, b as u32));
    }
    w.u64(stream.len() as u64);
    let code = write_histograms(w, 41, &[&tokens]);
    code.put_all(w, &tokens);
}

/// The value JPEG XL predicts for byte `i` of an ICC header.
fn icc_header_prediction(i: usize, size: u32, h: &[u8]) -> u8 {
    match i {
        0..=3 => size.to_be_bytes()[i],
        8 => 4,
        12..=23 => b"mntrRGB XYZ "[i - 12],
        36..=39 => b"acsp"[i - 36],
        41 | 42 if h[40] == b'A' => b'P',
        43 if h[40] == b'A' => b'L',
        41 if h[40] == b'M' => b'S',
        42 if h[40] == b'M' => b'F',
        43 if h[40] == b'M' => b'T',
        42 if h[40] == b'S' && h[41] == b'G' => b'I',
        43 if h[40] == b'S' && h[41] == b'G' => b' ',
        42 if h[40] == b'S' && h[41] == b'U' => b'N',
        43 if h[40] == b'S' && h[41] == b'U' => b'W',
        70 => 246,
        71 => 214,
        73 => 1,
        78 => 211,
        79 => 45,
        80..=83 => h[4 + i - 80],
        _ => 0,
    }
}

// ---------------------------------------------------------------- bit writer

/// Bits written least significant first, as JPEG XL reads them.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, nbits: u32, v: u64) {
        debug_assert!(nbits <= 32 && (nbits == 32 || v < 1u64 << nbits));
        self.acc |= v << self.n;
        self.n += nbits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn bit(&mut self, b: bool) {
        self.put(1, b as u64);
    }

    fn len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    fn pad(&mut self) {
        if self.n > 0 {
            self.put(8 - self.n, 0);
        }
    }

    fn append(&mut self, o: &Bits) {
        for &b in &o.out {
            self.put(8, b as u64);
        }
        self.put(o.n, o.acc);
    }

    /// A U32 field: a 2-bit selector, then the bits of the chosen
    /// distribution, each `(offset, bits)`.
    fn u32(&mut self, v: u32, dist: [(u32, u32); 4]) {
        for (sel, &(off, bits)) in dist.iter().enumerate() {
            if v >= off && ((v - off) as u64) < 1u64 << bits {
                self.put(2, sel as u64);
                self.put(bits, (v - off) as u64);
                return;
            }
        }
        unreachable!("value {v} does not fit its field");
    }

    fn u64(&mut self, v: u64) {
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

    fn f16(&mut self, v: f32) {
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

fn ceil_log2(x: usize) -> u32 {
    if x <= 1 {
        0
    } else {
        usize::BITS - (x - 1).leading_zeros()
    }
}

fn pack_signed(v: i32) -> u32 {
    if v >= 0 {
        (v as u32) << 1
    } else {
        ((-(v as i64)) as u32) * 2 - 1
    }
}

// ---------------------------------------------------------------- entropy coding

/// A value split into a prefix-coded token and raw bits: the hybrid
/// integer configuration with split exponent 4, two mantissa bits in the
/// token and none from the bottom.
fn hybrid(v: u32) -> (usize, u32, u32) {
    if v < 16 {
        return (v as usize, 0, 0);
    }
    let n = 31 - v.leading_zeros();
    let m = (v >> (n - 2)) & 3;
    let nb = n - 2;
    ((16 + ((n - 4) << 2) + m) as usize, nb, v & ((1 << nb) - 1))
}

/// Symbols to code: (context, value).
type Tokens = Vec<(u32, u32)>;

/// Entropy codes for a set of contexts, and the map from context to code.
struct Code {
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
    fn put_all(&self, w: &mut Bits, tokens: &[(u32, u32)]) {
        match &self.codes {
            Codes::Prefix { lens, codes } => {
                for &(c, v) in tokens {
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
                let mut out = Vec::with_capacity(tokens.len());
                for &(c, v) in tokens.iter().rev() {
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
fn write_context_map(w: &mut Bits, map: &[u8]) {
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
        let code = write_histograms(&mut s, 1, &[&tokens]);
        code.put_all(&mut s, &tokens);
        if best.as_ref().is_none_or(|b| s.len() < b.len()) {
            best = Some(s);
        }
    }
    w.append(&best.unwrap());
}

/// Clusters the contexts of `tokens`, writes the histograms (context map,
/// integer configurations and codes, prefix or ANS, whichever is smaller)
/// and returns the codes.
fn write_histograms(w: &mut Bits, num_ctx: usize, parts: &[&[(u32, u32)]]) -> Code {
    let mut hists: Vec<Vec<u32>> = vec![Vec::new(); num_ctx];
    for &(c, v) in parts.iter().flat_map(|t| t.iter()) {
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
fn write_ans_histogram(w: &mut Bits, h: &[u32], log_alpha: u32) -> Vec<u16> {
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

// ---------------------------------------------------------------- modular

const PRED_ZERO: u32 = 0;
const PRED_GRADIENT: u32 = 5;
const PRED_WEIGHTED: u32 = 6;
const PROP_STREAM: u32 = 1;
const PROP_WP: u32 = 15;

/// A meta-adaptive tree as built: a property threshold splits the values
/// above it to the left.
enum Node {
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

fn split(prop: u32, val: i32, left: Node, right: Node) -> Node {
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

struct Tree {
    nodes: Vec<Flat>,
    leaves: usize,
    wp: bool,
}

impl Tree {
    fn new(root: Node) -> Tree {
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
    fn tokens(&self) -> Tokens {
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
    pred: i64,
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
    fn new(xsize: usize) -> Self {
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
fn stream_tokens(
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
fn hist_cost(h: &[u32; 128]) -> f64 {
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
fn dc_tree(samples: &mut [(i32, i32, u8)], depth: u32) -> Node {
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
fn modular_header(w: &mut Bits) {
    w.bit(true); // global tree
    w.bit(true); // default weighted predictor parameters
    w.put(2, 0); // no transforms
}

// ---------------------------------------------------------------- frame

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
    map: Vec<u8>,
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
    fn new(fine: Vec<i32>, ks: [usize; 3]) -> Self {
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

    fn write(&self, w: &mut Bits) {
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
    fn get(&self, c: usize, fb: usize) -> u32 {
        let k = self.thresholds.len() + 1;
        let ch = if c < 2 { c ^ 1 } else { 2 };
        self.map[ch * NUM_ORDERS * k + fb * k / (self.fine.len() + 1)] as u32
    }
}
/// How many block contexts each channel gets (Y, Cb, Cr order): the
/// grouping of the `f` fine buckets with the least estimated cost, within
/// the format's limit of 16 in all. `ac` holds tokens with local
/// contexts, `wh` each one's JPEG XL channel (Cb, Y, Cr) and fine bucket.
fn choose_block_ctx(ac: &[Tokens], wh: &[Vec<u8>], f: usize) -> [usize; 3] {
    const LOCAL: usize = (NON_ZERO_BUCKETS + ZERO_DENSITY_CONTEXTS) as usize;
    let mut hist = vec![[0u32; 64]; 3 * 8 * LOCAL];
    for (t, w) in ac.iter().zip(wh) {
        for (&(ctx, v), &at) in t.iter().zip(w) {
            hist[at as usize * LOCAL + ctx as usize][hybrid(v).0.min(63)] += 1;
        }
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
    /// Blocks of the JPEG component, row stride `bw`, zigzag order.
    blocks: &'a [[i16; 64]],
    bw: usize,
    /// Quantization table in natural order.
    q: [u16; 64],
}

struct Frame<'a> {
    width: usize,
    height: usize,
    /// Channels in JPEG XL order: Cb, Y, Cr.
    ch: [Chan<'a>; 3],
    up: [u32; 3],
    xblocks: usize,
    yblocks: usize,
}

impl<'a> Frame<'a> {
    fn new(p: &'a Parsed) -> Option<Self> {
        if p.comps.len() != 3 || p.adobe == Some(0) {
            return None;
        }
        if !p.jfif && p.adobe.is_none() && p.ids == *b"RGB" {
            return None;
        }
        let mode = |h: usize, v: usize| match (h, v) {
            (1, 1) => Some(0u32),
            (2, 2) => Some(1),
            (2, 1) => Some(2),
            (1, 2) => Some(3),
            _ => None,
        };
        const HS: [usize; 4] = [0, 1, 1, 0];
        const VS: [usize; 4] = [0, 1, 0, 1];
        let jpeg_of = [1usize, 0, 2];
        let mut up = [0u32; 3];
        for c in 0..3 {
            let j = &p.comps[jpeg_of[c]];
            up[c] = mode(j.h, j.v)?;
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
            // The block grid must be the one JPEG XL derives.
            if j.bw < xblocks >> hs || j.blocks.len() < (yblocks >> vs) * j.bw {
                return None;
            }
            chans.push(Chan {
                hs,
                vs,
                blocks: &j.blocks,
                bw: j.bw,
                q,
            });
        }
        let ch: [Chan<'a>; 3] = chans.try_into().ok()?;
        Some(Frame {
            width,
            height,
            ch,
            up,
            xblocks,
            yblocks,
        })
    }

    fn is444(&self) -> bool {
        self.ch.iter().all(|c| c.hs == 0 && c.vs == 0)
    }

    fn encode(&self, meta: &Meta) -> Vec<u8> {
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
                        v.push(ch.blocks[(y0 + y) * ch.bw + x0 + x][0] as i32);
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
                luma_dc.push(luma.blocks[y * luma.bw + x][0]);
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
                    let blk = &ch.blocks[y * ch.bw + x];
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
        // AC tokens with contexts local to a block context (nonzero counts
        // first, then coefficients), and each token's channel and fine
        // bucket; the block contexts are chosen from them below.
        let fb = BlockCtx::new(fine.clone(), [1; 3]);
        let mut ac: Vec<Tokens> = Vec::new();
        let mut ac_where: Vec<Vec<u8>> = Vec::new();
        for g in 0..num_groups {
            let mut wh = Vec::new();
            let (ox, oy) = ((g % groups_x) * 32, (g / groups_x) * 32);
            let (gw, gh) = ((self.xblocks - ox).min(32), (self.yblocks - oy).min(32));
            let mut t = Tokens::new();
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
                        let blk = &ch.blocks[((oy >> ch.vs) + sby) * ch.bw + (ox >> ch.hs) + sbx];
                        let coef = |k: usize| blk[orders[c][k]] as i32;
                        let nonzeros = (1..64).filter(|&k| coef(k) != 0).count();
                        let dc = luma.blocks
                            [((oy + by) >> luma.vs) * luma.bw + ((ox + bx) >> luma.hs)][0];
                        let at = (c * 8 + fb.bucket(dc as i32)) as u8;
                        t.push((nonzero_context(predicted, 0, 1), nonzeros as u32));
                        wh.push(at);
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
                            t.push((ctx, pack_signed(v)));
                            wh.push(at);
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
            ac.push(t);
            ac_where.push(wh);
        }
        let bctx = BlockCtx::new(fine, choose_block_ctx(&ac, &ac_where, fb.fine.len() + 1));
        for (t, wh) in ac.iter_mut().zip(&ac_where) {
            for (tok, &at) in t.iter_mut().zip(wh) {
                let bc = bctx.get(at as usize / 8, at as usize % 8);
                tok.0 = if tok.0 < NON_ZERO_BUCKETS {
                    tok.0 * bctx.num + bc
                } else {
                    bctx.num * NON_ZERO_BUCKETS
                        + ZERO_DENSITY_CONTEXTS * bc
                        + (tok.0 - NON_ZERO_BUCKETS)
                };
            }
        }
        drop(ac_where);
        bctx.write(&mut lfg);
        lfg.bit(false); // color correlation: none (YCbCr)
        lfg.put(2, 0);
        lfg.put(16, 0);
        lfg.put(16, 0);
        lfg.put(8, 128);
        lfg.put(8, 128);
        lfg.bit(true); // global tree
        let tree_tokens = tree.tokens();
        let tc = write_histograms(&mut lfg, 6, &[&tree_tokens]);
        tc.put_all(&mut lfg, &tree_tokens);
        let all: Vec<&[(u32, u32)]> = std::iter::once(&qt_tokens)
            .chain(&lf_tokens)
            .chain(&meta_tokens)
            .map(|t| &t[..])
            .collect();
        let mc = write_histograms(&mut lfg, tree.leaves, &all);

        // ---- LF groups
        let mut lf_sections = Vec::new();
        for g in 0..num_lf {
            let mut s = Bits::default();
            s.put(2, 0); // no extra precision
            modular_header(&mut s);
            mc.put_all(&mut s, &lf_tokens[g]);
            let (bw, bh) = lf_rects[g];
            s.put(ceil_log2(bw * bh), (bw * bh - 1) as u64);
            modular_header(&mut s);
            mc.put_all(&mut s, &meta_tokens[g]);
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
                mc.put_all(&mut hfg, &qt_tokens);
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
            let code = write_histograms(&mut hfg, 8, &[&t]);
            code.put_all(&mut hfg, &t);
        } else {
            hfg.put(2, 2); // natural coefficient orders
        }
        let num_ac = bctx.num * (NON_ZERO_BUCKETS + ZERO_DENSITY_CONTEXTS);
        let all_ac: Vec<&[(u32, u32)]> = ac.iter().map(|t| &t[..]).collect();
        let acc = write_histograms(&mut hfg, num_ac as usize, &all_ac);

        // ---- pass groups
        let mut pass_sections = Vec::new();
        for t in &ac {
            let mut s = Bits::default();
            acc.put_all(&mut s, t);
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
            for s in &sections {
                w.append(s);
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
            w.put(2, 0); // RGB
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
    use super::*;

    fn check_ans(h: &[u32]) {
        let mut w = Bits::default();
        let f = write_ans_histogram(&mut w, h, 8);
        assert_eq!(f.iter().map(|&v| v as u32).sum::<u32>(), 4096);
        for (i, &c) in h.iter().enumerate() {
            assert_eq!(c > 0, f[i] > 0, "symbol {i}");
        }
    }

    #[test]
    fn ans_histograms_that_nearly_fill_or_overfill_the_table() {
        // One symbol with nearly all the mass rounds up to 4096.
        let mut h = vec![0u32; 40];
        h[0] = 283282;
        h[17] = 3;
        h[39] = 1;
        check_ans(&h);
        // Many rare symbols each rounded up to 1.
        let h: Vec<u32> = (0..128).map(|i| if i < 64 { 1000 } else { 1 }).collect();
        check_ans(&h);
        let h: Vec<u32> = (0..255).map(|i| if i == 0 { 1 << 30 } else { 1 }).collect();
        check_ans(&h);
    }

    #[test]
    fn a_zero_quantizer_is_refused() {
        let rgb: Vec<u8> = (0..32 * 32 * 3).map(|i| (i * 7 % 251) as u8).collect();
        let mut jpeg = crate::Encoder::new(80).encode_rgb(&rgb, 32, 32);
        assert!(jpeg_to_jxl(&jpeg).is_some());
        let dqt = jpeg.windows(2).position(|w| w == [0xFF, 0xDB]).unwrap();
        jpeg[dqt + 5 + 63] = 0;
        assert!(jpeg_to_jxl(&jpeg).is_none());
    }

    #[test]
    fn exif_comes_from_the_segment_the_orientation_does() {
        // TIFF with one IFD: no entries, or the orientation alone.
        let empty = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        let mut six = b"II*\0\x08\0\0\0\x01\0".to_vec();
        six.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        let rgb = [128u8; 16 * 16 * 3];
        let mut jpeg = crate::Encoder::new(80).exif(&six).encode_rgb(&rgb, 16, 16);
        // An Exif segment without an orientation, ahead of the real one.
        let at = jpeg.windows(2).position(|w| w == [0xFF, 0xE1]).unwrap();
        let mut seg = vec![0xFF, 0xE1, 0, (2 + 6 + empty.len()) as u8];
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&empty);
        jpeg.splice(at..at, seg);
        let p = parse_and_decode(&jpeg, false, &Limits::default()).unwrap();
        let meta = Meta::read(&jpeg, &p);
        assert_eq!(meta.orientation, 6);
        assert_eq!(meta.exif.as_deref(), Some(&six[..]));
    }
}
