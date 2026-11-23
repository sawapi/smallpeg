//! Reading a JPEG for reconstruction: its markers and segments, and a trace
//! of its entropy-coded data.

use super::{App, Huff, Quant, Recon, Scan, ScanBlocks, ScanComp, Tables, band};
use crate::jxl::frame::{gray_sampling, sampling_mode};
use crate::optimize::Parsed;

/// Most markers a box can list.
const MAX_MARKERS: usize = 16384;
/// Longest data after the end of image a box can carry.
const MAX_TAIL: usize = 4_260_096;
/// Symbols of a DC code: the magnitude categories 0 to 11.
const DC_SYMBOLS: usize = 12;

/// Markers a file may hold between SOI and EOI, by second byte.
fn known_marker(m: u8) -> bool {
    matches!(m, 0xC0..=0xC2 | 0xC4 | 0xD0..=0xD7 | 0xD9..=0xDB | 0xDD | 0xE0..=0xEF | 0xFE)
}

/// A Huffman code for decoding: by the next 16 bits, the length and
/// symbol of the code they start with (0 where there is none).
struct Lut(Vec<u16>);

impl Lut {
    fn new(counts: &[u8; 17], values: &[u8]) -> Option<Lut> {
        let mut lut = vec![0u16; 1 << 16];
        let (mut code, mut k) = (0usize, 0usize);
        for len in 1..=16 {
            for _ in 0..counts[len] {
                let first = code << (16 - len);
                let span = 1usize << (16 - len);
                lut.get_mut(first..first + span)?
                    .fill(((len as u16) << 8) | values[k] as u16);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Some(Lut(lut))
    }
}

/// Entropy-coded data, read as bits up to the next marker.
struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u64,
    /// Bits in `acc` not yet taken.
    n: u32,
    /// How many of them, the lowest, are zeros made up past the data.
    over: u32,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Reader {
            d,
            pos,
            acc: 0,
            n: 0,
            over: 0,
        }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let b = match self.d.get(self.pos) {
                Some(&0xFF) if self.d.get(self.pos + 1) == Some(&0) => {
                    self.pos += 2;
                    0xFF
                }
                Some(&b) if b != 0xFF => {
                    self.pos += 1;
                    b
                }
                // A marker, or the end: nothing more to read.
                _ => {
                    self.over += 8;
                    0
                }
            };
            self.acc = (self.acc << 8) | b as u64;
            self.n += 8;
        }
    }

    fn bits(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        if self.n < count {
            self.fill();
        }
        self.n -= count;
        ((self.acc >> self.n) & ((1 << count) - 1)) as u32
    }

    fn symbol(&mut self, lut: &Lut) -> Option<u8> {
        if self.n < 16 {
            self.fill();
        }
        let e = lut.0[((self.acc >> (self.n - 16)) & 0xFFFF) as usize];
        if e == 0 {
            return None;
        }
        self.n -= (e >> 8) as u32;
        Some(e as u8)
    }

    /// Ends the data at the byte boundary: the padding bits go to
    /// `padding`, and the position after the last byte used comes back.
    /// `None` if more was read than there is.
    fn finish(mut self, padding: &mut Vec<bool>, zero: &mut bool) -> Option<usize> {
        let real = self.n.checked_sub(self.over)?;
        let pad = real & 7;
        for i in (0..pad).rev() {
            let bit = (self.acc >> (self.n - pad + i)) & 1 == 1;
            *zero |= !bit;
            padding.push(bit);
        }
        // Give back the whole bytes not used, each one or (0xFF 0x00) two.
        for _ in 0..real >> 3 {
            let stuffed =
                self.pos >= 2 && self.d[self.pos - 1] == 0 && self.d[self.pos - 2] == 0xFF;
            self.pos -= if stuffed { 2 } else { 1 };
        }
        Some(self.pos)
    }
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some((*d.get(at)? as usize) << 8 | *d.get(at + 1)? as usize)
}

/// Reads the file's structure, tracing its entropy-coded data.
pub(super) fn parse<'a>(d: &'a [u8], p: &Parsed) -> Option<Recon<'a>> {
    let n = p.comps.len();
    if !d.starts_with(&[0xFF, 0xD8]) || !(n == 1 || n == 3) || p.ids.len() != n {
        return None;
    }
    // One component keeps the factors its header has, when a frame can
    // carry them, although the image is the same with any.
    let sampling: Vec<u8> = if n == 1 {
        let (h, v) = gray_sampling(d, p);
        sampling_mode(h, v)?;
        vec![(h << 4 | v) as u8]
    } else {
        p.comps.iter().map(|c| (c.h << 4 | c.v) as u8).collect()
    };
    let mut r = Recon {
        markers: Vec::new(),
        apps: Vec::new(),
        coms: Vec::new(),
        quant: Vec::new(),
        comp_quant: Vec::new(),
        sampling,
        huff: Vec::new(),
        scans: Vec::new(),
        restart: None,
        inter: Vec::new(),
        tail: &[],
        padding: Vec::new(),
        zero_padding: false,
    };
    let mut luts: Tables<Lut> = Tables::default();
    let mut progressive = None;
    let mut pos = 2;
    loop {
        let start = pos;
        while pos + 1 < d.len() && !(d[pos] == 0xFF && known_marker(d[pos + 1])) {
            pos += 1;
        }
        if pos > start {
            if pos - start > 0xFFFF {
                return None;
            }
            r.markers.push(0xFF);
            r.inter.push(&d[start..pos]);
        }
        if pos + 1 >= d.len() {
            return None;
        }
        let m = d[pos + 1];
        pos += 2;
        // A segment with a length: its end.
        let segment = |pos: usize| {
            let len = be16(d, pos)?;
            (len >= 2 && pos + len <= d.len()).then_some(pos + len)
        };
        match m {
            0xC0..=0xC2 => {
                if progressive.is_some() {
                    return None;
                }
                progressive = Some(m == 0xC2);
                pos = segment(pos)?;
            }
            0xC4 => {
                let end = segment(pos)?;
                pos += 2;
                if pos == end {
                    r.huff.push(Huff {
                        slot: 0,
                        last: true,
                        counts: [0; 17],
                        values: Vec::new(),
                        empty: true,
                    });
                }
                while pos < end {
                    let slot = *d.get(pos)?;
                    let (ac, id) = (slot >> 4, (slot & 15) as usize);
                    if ac > 1 || id > 3 || pos + 17 > end {
                        return None;
                    }
                    let mut counts = [0u8; 17];
                    counts[1..].copy_from_slice(&d[pos + 1..pos + 17]);
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    pos += 17;
                    if pos + total > end || total > if ac == 1 { 256 } else { DC_SYMBOLS } {
                        return None;
                    }
                    let values = d[pos..pos + total].to_vec();
                    pos += total;
                    // No symbol twice, DC categories only in a DC code,
                    // and room left for one more code of the longest
                    // length (the box counts one there).
                    let mut seen = [false; 256];
                    for &v in &values {
                        if std::mem::replace(&mut seen[v as usize], true)
                            || (ac == 0 && v as usize >= DC_SYMBOLS)
                        {
                            return None;
                        }
                    }
                    let longest = (1..=16).rev().find(|&l| counts[l] != 0).unwrap_or(1);
                    let space: usize = (1..=16).map(|l| (counts[l] as usize) << (16 - l)).sum();
                    // (With that one the count has to fit the box's field.)
                    if space + (1 << (16 - longest)) > 1 << 16 || counts[longest] == 255 {
                        return None;
                    }
                    let lut = Lut::new(&counts, &values)?;
                    if ac == 1 {
                        luts.ac[id] = Some(lut);
                    } else {
                        luts.dc[id] = Some(lut);
                    }
                    r.huff.push(Huff {
                        slot,
                        last: pos == end,
                        counts,
                        values,
                        empty: false,
                    });
                }
            }
            0xD0..=0xD7 | 0xD9 => {}
            0xDA => {
                let end = segment(pos)?;
                let n = *d.get(pos + 2)? as usize;
                if !(1..=3).contains(&n) || pos + 3 + 2 * n + 3 != end {
                    return None;
                }
                let mut comps: Vec<ScanComp> = Vec::new();
                for i in 0..n {
                    let (id, tables) = (d[pos + 3 + 2 * i], d[pos + 4 + 2 * i]);
                    let comp = p.ids.iter().position(|&x| x == id)?;
                    let (dc, ac) = ((tables >> 4) as usize, (tables & 15) as usize);
                    if comps.iter().any(|c| c.comp == comp) || dc > 3 || ac > 3 {
                        return None;
                    }
                    comps.push(ScanComp { comp, dc, ac });
                }
                let (ss, se, a) = (d[end - 3], d[end - 2], d[end - 1]);
                if ss > 63 || se > 63 || se < ss {
                    return None;
                }
                let mut scan = Scan {
                    comps,
                    ss,
                    se,
                    al: a & 15,
                    ah: a >> 4,
                    resets: Vec::new(),
                    zero_runs: Vec::new(),
                };
                pos = trace(d, end, p, progressive?, &luts, &mut scan, &mut r)?;
                r.scans.push(scan);
            }
            0xDB => {
                let end = segment(pos)?;
                pos += 2;
                if pos == end {
                    return None;
                }
                while pos < end {
                    let t = d[pos];
                    let (wide, index) = (t >> 4, t & 15);
                    let len = 64 * (wide as usize + 1);
                    if wide > 1 || index > 3 || pos + 1 + len > end || r.quant.len() == 3 {
                        return None;
                    }
                    pos += 1 + len;
                    r.quant.push(Quant {
                        wide: wide == 1,
                        index,
                        last: pos == end,
                    });
                }
            }
            0xDD => {
                if r.restart.is_some() || segment(pos)? != pos + 4 {
                    return None;
                }
                r.restart = Some(be16(d, pos + 2)? as u16);
                pos += 4;
            }
            0xE0..=0xEF => {
                let end = segment(pos)?;
                r.apps.push((&d[pos - 1..end], App::Unknown));
                pos = end;
            }
            0xFE => {
                let end = segment(pos)?;
                r.coms.push(&d[pos - 1..end]);
                pos = end;
            }
            _ => return None,
        }
        r.markers.push(m);
        if r.markers.len() > MAX_MARKERS {
            return None;
        }
        if m == 0xD9 {
            break;
        }
    }
    r.tail = &d[pos..];
    if r.tail.len() > MAX_TAIL || r.scans.is_empty() || r.quant.is_empty() {
        return None;
    }
    // The box numbers the Huffman codes and the padding in fields this wide.
    if !(2..=89).contains(&r.huff.len()) || (r.zero_padding && r.padding.len() >= 1 << 24) {
        return None;
    }
    for c in &p.comps {
        r.comp_quant
            .push(r.quant.iter().position(|q| q.index == c.tq)?);
    }
    Some(r)
}

/// Reads through the entropy-coded data of a scan that starts at `pos`,
/// noting its padding and where its coding is not the plain one. Returns
/// the position after it.
fn trace(
    d: &[u8],
    pos: usize,
    p: &Parsed,
    progressive: bool,
    luts: &Tables<Lut>,
    scan: &mut Scan,
    r: &mut Recon,
) -> Option<usize> {
    let (ss, se, al, ah) = band(progressive, scan);
    if al > 10 {
        return None;
    }
    let blocks = ScanBlocks::new(p, &scan.comps)?;
    let tables: Vec<(Option<&Lut>, Option<&Lut>)> = scan
        .comps
        .iter()
        .map(|c| (luts.dc[c.dc].as_ref(), luts.ac[c.ac].as_ref()))
        .collect();
    // By the scan header as written, whatever the frame makes of it.
    if tables
        .iter()
        .any(|t| (scan.ss == 0 && t.0.is_none()) || (scan.se > 0 && t.1.is_none()))
    {
        return None;
    }
    let interval = r.restart.unwrap_or(0) as usize;
    let eob_runs = ss > 0;
    let mut rd = Reader::new(d, pos);
    let (mut to_go, mut next_rst) = (interval, 0u8);
    // Blocks left in the end-of-band run; below zero when none has been
    // coded since the last coefficient, -1 at a fresh start.
    let mut eobrun: i64 = -1;
    let mut index = 0u32;
    let mut failed = false;
    let (mut resets, mut zero_runs) = (Vec::new(), Vec::new());
    for my in 0..blocks.mcus_y {
        for mx in 0..blocks.mcus_x {
            if interval > 0 {
                if to_go == 0 {
                    let at = rd.finish(&mut r.padding, &mut r.zero_padding)?;
                    if d.get(at) != Some(&0xFF) || d.get(at + 1) != Some(&(0xD0 + next_rst)) {
                        return None;
                    }
                    if eobrun > 0 {
                        return None;
                    }
                    rd = Reader::new(d, at + 2);
                    next_rst = (next_rst + 1) & 7;
                    to_go = interval;
                    eobrun = -1;
                }
                to_go -= 1;
            }
            blocks.each(mx, my, &mut |i, bi| {
                if failed {
                    return;
                }
                let (dc, ac) = tables[i];
                let traced = if ah == 0 {
                    trace_first(&mut rd, dc, ac, ss, se, al, eob_runs, &mut eobrun)
                } else {
                    let blk = &p.comps[scan.comps[i].comp].blocks[bi];
                    trace_refine(&mut rd, ac, ss, se, al, eob_runs, &mut eobrun, blk)
                        .map(|x| (x, 0))
                };
                match traced {
                    Some((reset, zeros)) => {
                        if reset {
                            resets.push(index);
                        }
                        if zeros > 0 {
                            zero_runs.push((index, zeros));
                        }
                        failed |= zeros > 4;
                    }
                    None => failed = true,
                }
                index += 1;
            });
            if failed {
                return None;
            }
        }
    }
    if eobrun > 0 || resets.len() > 65555 || zero_runs.len() > 65555 {
        return None;
    }
    scan.resets = resets;
    scan.zero_runs = zero_runs;
    rd.finish(&mut r.padding, &mut r.zero_padding)
}

/// An end-of-band symbol with run length code `r`, at coefficient `k` of
/// a band starting at `ss`. Returns whether it restarts a run that could
/// have gone on.
fn start_eobrun(
    rd: &mut Reader,
    r: u32,
    at_start: bool,
    eob_runs: bool,
    eobrun: &mut i64,
) -> Option<bool> {
    let reset = eob_runs && at_start && *eobrun == 0;
    *eobrun = 1 << r;
    if r > 0 {
        if !eob_runs {
            return None;
        }
        *eobrun += rd.bits(r) as i64;
    }
    Some(reset)
}

/// One block of a first scan of its band (or of a sequential scan):
/// whether it cuts an end-of-band run short, and how many runs of
/// sixteen zeros it ends with.
#[allow(clippy::too_many_arguments)]
fn trace_first(
    rd: &mut Reader,
    dc: Option<&Lut>,
    ac: Option<&Lut>,
    mut ss: usize,
    se: usize,
    al: u32,
    eob_runs: bool,
    eobrun: &mut i64,
) -> Option<(bool, u32)> {
    if ss == 0 {
        let s = rd.symbol(dc?)? as u32;
        rd.bits(s);
        ss = 1;
    }
    if ss > se {
        return Some((false, 0));
    }
    if *eobrun > 0 {
        *eobrun -= 1;
        return Some((false, 0));
    }
    let ac = ac?;
    let (mut reset, mut zeros) = (false, 0);
    let mut k = ss;
    while k <= se {
        let sym = rd.symbol(ac)?;
        let (run, size) = ((sym >> 4) as usize, (sym & 15) as u32);
        if size > 0 {
            k += run;
            if k > se || size + al >= DC_SYMBOLS as u32 {
                return None;
            }
            rd.bits(size);
            zeros = 0;
        } else if run == 15 {
            k += 15;
            zeros += 1;
        } else {
            reset = start_eobrun(rd, run as u32, k == ss, eob_runs, eobrun)?;
            break;
        }
        k += 1;
    }
    *eobrun -= 1;
    Some((reset, zeros))
}

/// One block of a scan that refines its band by a bit: whether it cuts
/// an end-of-band run short. `blk` is the block as the whole file leaves
/// it, which tells the coefficients that were nonzero before this scan.
#[allow(clippy::too_many_arguments)]
fn trace_refine(
    rd: &mut Reader,
    ac: Option<&Lut>,
    mut ss: usize,
    se: usize,
    al: u32,
    eob_runs: bool,
    eobrun: &mut i64,
    blk: &[i16; 64],
) -> Option<bool> {
    if ss == 0 {
        rd.bits(1);
        ss = 1;
    }
    if ss > se {
        return Some(false);
    }
    let known = |k: usize| (blk[k].unsigned_abs() >> al) > 1;
    let mut reset = false;
    let mut k = ss;
    if *eobrun <= 0 {
        let ac = ac?;
        let mut in_zero_run = false;
        while k <= se {
            let sym = rd.symbol(ac)?;
            let (mut run, size) = ((sym >> 4) as i32, sym & 15);
            if size != 0 {
                if size != 1 {
                    return None;
                }
                rd.bits(1);
                in_zero_run = false;
            } else if run != 15 {
                reset = start_eobrun(rd, run as u32, k == ss, eob_runs, eobrun)?;
                break;
            } else {
                in_zero_run = true;
            }
            loop {
                if known(k) {
                    rd.bits(1);
                } else {
                    run -= 1;
                    if run < 0 {
                        break;
                    }
                }
                k += 1;
                if k > se {
                    break;
                }
            }
            if size != 0 && k > se {
                return None;
            }
            k += 1;
        }
        if in_zero_run {
            return None;
        }
    }
    if *eobrun > 0 {
        while k <= se {
            if known(k) {
                rd.bits(1);
            }
            k += 1;
        }
    }
    *eobrun -= 1;
    Some(reset)
}
