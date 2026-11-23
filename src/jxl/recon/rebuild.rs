//! Writing the JPEG again from the reconstruction data and the image, the
//! way a decoder does, to see that it comes out as the file.

use super::{Huff, Recon, Scan, ScanBlocks, Tables, band};
use crate::optimize::Parsed;

/// A Huffman code for coding: the code and length of each symbol (length
/// 0 where the code has none).
struct Code {
    code: [u16; 256],
    len: [u8; 256],
}

impl Code {
    fn new(h: &Huff) -> Code {
        let mut c = Code {
            code: [0; 256],
            len: [0; 256],
        };
        let (mut code, mut k) = (0u32, 0);
        for len in 1..=16 {
            for _ in 0..h.counts[len] {
                c.code[h.values[k] as usize] = code as u16;
                c.len[h.values[k] as usize] = len as u8;
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        c
    }
}

/// The file being written again, held against the original: nothing is
/// kept, only whether every byte so far was the same.
pub(super) struct Out<'a> {
    pub(super) d: &'a [u8],
    pub(super) pos: usize,
    pub(super) same: bool,
    /// Entropy-coded bits not yet making a byte.
    pub(super) acc: u64,
    pub(super) n: u32,
    /// Tests keep the bytes instead, to make files from changed data.
    #[cfg(test)]
    pub(super) kept: Option<Vec<u8>>,
}

impl Out<'_> {
    fn bytes(&mut self, b: &[u8]) {
        #[cfg(test)]
        if let Some(kept) = &mut self.kept {
            kept.extend_from_slice(b);
            return;
        }
        self.same &= self.d[self.pos.min(self.d.len())..].starts_with(b);
        self.pos += b.len();
    }

    fn put(&mut self, count: u32, bits: u32) {
        self.acc = (self.acc << count) | bits as u64;
        self.n += count;
        while self.n >= 8 {
            self.n -= 8;
            let b = (self.acc >> self.n) as u8;
            self.bytes(if b == 0xFF {
                &[0xFF, 0]
            } else {
                std::slice::from_ref(&b)
            });
        }
    }

    fn symbol(&mut self, code: &Code, sym: usize) {
        self.same &= code.len[sym] != 0;
        self.put(code.len[sym] as u32, code.code[sym] as u32);
    }
}

/// The end-of-band run and the refinement bits waiting behind it.
#[derive(Default)]
struct Pending<'c> {
    eobrun: u32,
    code: Option<&'c Code>,
    bits: Vec<bool>,
}

impl<'c> Pending<'c> {
    fn flush(&mut self, out: &mut Out) {
        if self.eobrun > 0 {
            let size = 31 - self.eobrun.leading_zeros();
            match self.code {
                Some(code) => out.symbol(code, (size << 4) as usize),
                None => out.same = false,
            }
            out.put(size, self.eobrun & ((1 << size) - 1));
            self.eobrun = 0;
        }
        for &b in &self.bits {
            out.put(1, b as u32);
        }
        self.bits.clear();
    }

    fn end_of_band(&mut self, code: &'c Code, bits: &[bool], out: &mut Out) {
        if self.eobrun == 0 {
            self.code = Some(code);
        }
        self.eobrun += 1;
        self.bits.extend_from_slice(bits);
        if self.eobrun == 0x7FFF {
            self.flush(out);
        }
    }
}

/// The size category of a value and the bits that follow its symbol.
fn magnitude(v: i32) -> (u32, u32) {
    let size = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 { v - 1 } else { v } as u32;
    (size, bits & ((1u32 << size) - 1))
}

/// Writes the file again from `r` and the image, as a decoder would, and
/// says whether that is the file.
pub(super) fn rebuilds(r: &Recon, d: &[u8], p: &Parsed) -> bool {
    let mut out = Out {
        d,
        pos: 0,
        same: true,
        acc: 0,
        n: 0,
        #[cfg(test)]
        kept: None,
    };
    rebuild(r, p, &mut out) && out.pos == d.len()
}

pub(super) fn rebuild(r: &Recon, p: &Parsed, out: &mut Out) -> bool {
    out.bytes(&[0xFF, 0xD8]);
    let mut codes: Tables<Code> = Tables::default();
    let (mut apps, mut coms, mut inter) = (r.apps.iter(), r.coms.iter(), r.inter.iter());
    let (mut quant, mut huff, mut scans) = (0, 0, r.scans.iter());
    let mut progressive = false;
    let mut interval = 0;
    let mut padding = r.padding.iter();
    for &m in &r.markers {
        match m {
            0xC0..=0xC2 => {
                progressive = m == 0xC2;
                let n = p.comps.len() as u8;
                let mut s = vec![0xFF, m, 0, 8 + 3 * n, 8];
                s.extend_from_slice(&p.height.to_be_bytes());
                s.extend_from_slice(&p.width.to_be_bytes());
                s.push(n);
                for i in 0..p.comps.len() {
                    s.extend_from_slice(&[p.ids[i], r.sampling[i], r.quant[r.comp_quant[i]].index]);
                }
                out.bytes(&s);
            }
            0xC4 => {
                let mut body = Vec::new();
                loop {
                    let Some(h) = r.huff.get(huff) else {
                        return false;
                    };
                    huff += 1;
                    if h.empty {
                        break;
                    }
                    body.push(h.slot);
                    body.extend_from_slice(&h.counts[1..]);
                    body.extend_from_slice(&h.values);
                    let id = (h.slot & 15) as usize;
                    if h.slot & 0x10 != 0 {
                        codes.ac[id] = Some(Code::new(h));
                    } else {
                        codes.dc[id] = Some(Code::new(h));
                    }
                    if h.last {
                        break;
                    }
                }
                out.bytes(&[0xFF, 0xC4]);
                out.bytes(&((body.len() + 2) as u16).to_be_bytes());
                out.bytes(&body);
            }
            0xD0..=0xD7 => out.bytes(&[0xFF, m]),
            0xD9 => {
                out.bytes(&[0xFF, 0xD9]);
                out.bytes(r.tail);
            }
            0xDA => {
                let Some(scan) = scans.next() else {
                    return false;
                };
                if !rebuild_scan(r, p, scan, progressive, interval, &codes, &mut padding, out) {
                    return false;
                }
            }
            0xDB => {
                let mut body = Vec::new();
                loop {
                    let Some(q) = r.quant.get(quant) else {
                        return false;
                    };
                    // The table a decoder has here: that of a component
                    // using it, or else the one before it again.
                    let used = |j: usize| r.comp_quant.iter().position(|&q| q == j);
                    let Some(c) = (0..=quant).rev().find_map(used) else {
                        return false;
                    };
                    let Some(table) = &p.qts[p.comps[c].tq as usize] else {
                        return false;
                    };
                    quant += 1;
                    body.push(((q.wide as u8) << 4) | q.index);
                    for &v in table {
                        if q.wide {
                            body.push((v >> 8) as u8);
                        }
                        body.push(v as u8);
                    }
                    if q.last {
                        break;
                    }
                }
                out.bytes(&[0xFF, 0xDB]);
                out.bytes(&((body.len() + 2) as u16).to_be_bytes());
                out.bytes(&body);
            }
            0xDD => {
                let Some(v) = r.restart else { return false };
                interval = v as usize;
                out.bytes(&[0xFF, 0xDD, 0, 4]);
                out.bytes(&v.to_be_bytes());
            }
            0xE0..=0xEF => {
                let Some((app, _)) = apps.next() else {
                    return false;
                };
                out.bytes(&[0xFF]);
                out.bytes(app);
            }
            0xFE => {
                let Some(com) = coms.next() else { return false };
                out.bytes(&[0xFF]);
                out.bytes(com);
            }
            0xFF => {
                let Some(data) = inter.next() else {
                    return false;
                };
                out.bytes(data);
            }
            _ => return false,
        }
        if !out.same {
            return false;
        }
    }
    // Stored padding has to come out even; all ones is never looked at.
    out.same && (!r.zero_padding || padding.next().is_none())
}

/// Writes one scan: its header and entropy-coded data.
#[allow(clippy::too_many_arguments)]
fn rebuild_scan(
    r: &Recon,
    p: &Parsed,
    scan: &Scan,
    progressive: bool,
    interval: usize,
    codes: &Tables<Code>,
    padding: &mut std::slice::Iter<bool>,
    out: &mut Out,
) -> bool {
    let n = scan.comps.len();
    let mut head = vec![0xFF, 0xDA, 0, (6 + 2 * n) as u8, n as u8];
    for c in &scan.comps {
        head.extend_from_slice(&[p.ids[c.comp], ((c.dc << 4) | c.ac) as u8]);
    }
    head.extend_from_slice(&[scan.ss, scan.se, (scan.ah << 4) | scan.al]);
    out.bytes(&head);

    let (ss, se, al, ah) = band(progressive, scan);
    let Some(blocks) = ScanBlocks::new(p, &scan.comps) else {
        return false;
    };
    let sequential = !progressive || (ah == 0 && al == 0 && ss == 0 && se == 63);
    let (want_dc, want_ac) = (ss == 0, ss != 0 || se != 0);
    let mut tables = Vec::new();
    for c in &scan.comps {
        let (dc, ac) = (codes.dc[c.dc].as_ref(), codes.ac[c.ac].as_ref());
        if (want_dc && dc.is_none()) || (want_ac && ac.is_none()) {
            return false;
        }
        tables.push((dc, ac));
    }
    // Ends the bits at a byte with the file's padding, or ones.
    let pad = |out: &mut Out, padding: &mut std::slice::Iter<bool>| {
        for _ in 0..(8 - out.n) % 8 {
            let bit = if r.zero_padding {
                match padding.next() {
                    Some(&b) => b,
                    None => {
                        out.same = false;
                        true
                    }
                }
            } else {
                true
            };
            out.put(1, bit as u32);
        }
    };
    let mut pending = Pending::default();
    let mut last_dc = [0i32; 3];
    let (mut to_go, mut next_rst) = (interval, 0u8);
    let (mut resets, mut zero_runs) = (
        scan.resets.iter().peekable(),
        scan.zero_runs.iter().peekable(),
    );
    let mut index = 0u32;
    for my in 0..blocks.mcus_y {
        for mx in 0..blocks.mcus_x {
            if interval > 0 && to_go == 0 {
                pending.flush(out);
                pad(out, padding);
                out.bytes(&[0xFF, 0xD0 + next_rst]);
                next_rst = (next_rst + 1) & 7;
                to_go = interval;
                last_dc = [0; 3];
            }
            blocks.each(mx, my, &mut |i, bi| {
                let comp = scan.comps[i].comp;
                let blk = &p.comps[comp].blocks[bi];
                let (dc, ac) = tables[i];
                if resets.next_if_eq(&&index).is_some() {
                    pending.flush(out);
                }
                let zeros = zero_runs.next_if(|z| z.0 == index).map_or(0, |z| z.1);
                let dc_last = &mut last_dc[comp];
                if sequential {
                    block_sequential(blk, dc, ac, zeros, dc_last, out);
                } else if ah == 0 {
                    block_first(blk, dc, ac, ss, se, al, zeros, &mut pending, dc_last, out);
                } else {
                    block_refine(blk, ac, ss, se, al, &mut pending, out);
                }
                index += 1;
            });
            if !out.same {
                return false;
            }
            to_go = to_go.wrapping_sub(1);
        }
    }
    pending.flush(out);
    pad(out, padding);
    out.same
}

fn block_sequential(
    blk: &[i16; 64],
    dc: Option<&Code>,
    ac: Option<&Code>,
    zeros: u32,
    last_dc: &mut i32,
    out: &mut Out,
) {
    let (Some(dc), Some(ac)) = (dc, ac) else {
        out.same = false;
        return;
    };
    let (size, bits) = magnitude(blk[0] as i32 - *last_dc);
    *last_dc = blk[0] as i32;
    out.symbol(dc, size as usize);
    out.put(size, bits);
    let mut run = 0i32;
    for &v in &blk[1..] {
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            out.symbol(ac, 0xF0);
            run -= 16;
        }
        let (size, bits) = magnitude(v as i32);
        out.symbol(ac, ((run as usize) << 4) + size as usize);
        out.put(size, bits);
        run = 0;
    }
    for _ in 0..zeros {
        out.symbol(ac, 0xF0);
        run -= 16;
    }
    if run > 0 {
        out.symbol(ac, 0);
    }
}

#[allow(clippy::too_many_arguments)]
fn block_first<'c>(
    blk: &[i16; 64],
    dc: Option<&Code>,
    ac: Option<&'c Code>,
    mut ss: usize,
    se: usize,
    al: u32,
    zeros: u32,
    pending: &mut Pending<'c>,
    last_dc: &mut i32,
    out: &mut Out,
) {
    let eob_runs = ss > 0;
    if ss == 0 {
        let Some(dc) = dc else {
            out.same = false;
            return;
        };
        let v = (blk[0] as i32) >> al;
        let (size, bits) = magnitude(v - *last_dc);
        *last_dc = v;
        out.symbol(dc, size as usize);
        out.put(size, bits);
        ss = 1;
    }
    if ss > se {
        return;
    }
    let Some(ac) = ac else {
        out.same = false;
        return;
    };
    let mut run = 0i32;
    for &c in &blk[ss..=se] {
        let v = (c.unsigned_abs() >> al) as i32;
        if v == 0 {
            run += 1;
            continue;
        }
        pending.flush(out);
        while run > 15 {
            out.symbol(ac, 0xF0);
            run -= 16;
        }
        let (size, bits) = magnitude(if c < 0 { -v } else { v });
        out.symbol(ac, ((run as usize) << 4) + size as usize);
        out.put(size, bits);
        run = 0;
    }
    if zeros > 0 {
        pending.flush(out);
        for _ in 0..zeros {
            out.symbol(ac, 0xF0);
            run -= 16;
        }
    }
    if run > 0 {
        pending.end_of_band(ac, &[], out);
        if !eob_runs {
            pending.flush(out);
        }
    }
}

fn block_refine<'c>(
    blk: &[i16; 64],
    ac: Option<&'c Code>,
    mut ss: usize,
    se: usize,
    al: u32,
    pending: &mut Pending<'c>,
    out: &mut Out,
) {
    let eob_runs = ss > 0;
    if ss == 0 {
        out.put(1, ((blk[0] as i32 >> al) & 1) as u32);
        ss = 1;
    }
    if ss > se {
        return;
    }
    let Some(ac) = ac else {
        out.same = false;
        return;
    };
    let abs = |k: usize| blk[k].unsigned_abs() >> al;
    // The last coefficient that becomes nonzero in this scan.
    let eob = (ss..=se).rev().find(|&k| abs(k) == 1).unwrap_or(0);
    let mut run = 0;
    let mut bits: Vec<bool> = Vec::new();
    for k in ss..=se {
        let a = abs(k);
        if a == 0 {
            run += 1;
            continue;
        }
        while run > 15 && k <= eob {
            pending.flush(out);
            out.symbol(ac, 0xF0);
            run -= 16;
            for b in bits.drain(..) {
                out.put(1, b as u32);
            }
        }
        if a > 1 {
            bits.push(a & 1 == 1);
            continue;
        }
        pending.flush(out);
        out.symbol(ac, (run << 4) + 1);
        out.put(1, (blk[k] >= 0) as u32);
        for b in bits.drain(..) {
            out.put(1, b as u32);
        }
        run = 0;
    }
    if run > 0 || !bits.is_empty() {
        pending.end_of_band(ac, &bits, out);
        if !eob_runs {
            pending.flush(out);
        }
    }
}
