//! Reconstruction data: what a decoder needs, besides the image, to write
//! the original JPEG file back byte for byte (the `jbrd` box of ISO/IEC
//! 18181-2).
//!
//! The coefficients, quantization tables, size and sampling come from the
//! codestream, the color profile from its header, Exif and XMP from their
//! boxes. This adds the rest: the order of the markers, the Huffman codes
//! and scans as the file has them, and the places where its entropy coding
//! departs from what a decoder would write on its own (padding bits,
//! end-of-band runs cut short, runs of zeros before an end of block).
//!
//! The file is read here marker by marker and then written again from
//! exactly what the box will hold, the way a decoder does it. Only when
//! that gives the same bytes is there a box.

use super::{Bits, Meta};
use crate::optimize::Parsed;

/// Most markers a box can list.
const MAX_MARKERS: usize = 16384;
/// Longest data after the end of image a box can carry.
const MAX_TAIL: usize = 4_260_096;
/// Symbols of a DC code: the magnitude categories 0 to 11.
const DC_SYMBOLS: usize = 12;

#[derive(Clone, Copy, PartialEq)]
enum App {
    /// Stored in the box as it is.
    Unknown = 0,
    /// A chunk of the color profile in the codestream.
    Icc = 1,
    /// The contents of the Exif box.
    Exif = 2,
    /// The contents of the XMP box.
    Xmp = 3,
}

/// One Huffman code of a DHT segment. An empty segment is a code with no
/// symbols that ends it.
struct Huff {
    /// Class (0x10 for AC) and destination, as in the file.
    slot: u8,
    /// The last code of its segment.
    last: bool,
    /// Codes of each length, 1 to 16.
    counts: [u8; 17],
    values: Vec<u8>,
    /// No code at all: a DHT segment without tables.
    empty: bool,
}

struct ScanComp {
    comp: usize,
    dc: usize,
    ac: usize,
}

struct Scan {
    comps: Vec<ScanComp>,
    ss: u8,
    se: u8,
    al: u8,
    ah: u8,
    /// Blocks, counted in scan order, before which the end-of-band run
    /// so far was written out although it could have gone on.
    resets: Vec<u32>,
    /// Blocks that end with runs of sixteen zeros nothing follows, and
    /// how many.
    zero_runs: Vec<(u32, u32)>,
}

struct Quant {
    /// 16-bit entries.
    wide: bool,
    index: u8,
    /// The last table of its DQT segment.
    last: bool,
}

struct Recon<'a> {
    /// Second bytes of the markers after SOI, to EOI; 0xFF stands for
    /// bytes between markers that are no marker.
    markers: Vec<u8>,
    /// APPn segments from their marker byte on.
    apps: Vec<(&'a [u8], App)>,
    /// COM segments from their marker byte on.
    coms: Vec<&'a [u8]>,
    quant: Vec<Quant>,
    /// Which of `quant` each component uses.
    comp_quant: Vec<usize>,
    huff: Vec<Huff>,
    scans: Vec<Scan>,
    restart: Option<u16>,
    inter: Vec<&'a [u8]>,
    tail: &'a [u8],
    /// Every bit that pads the entropy-coded data to a byte, in order.
    padding: Vec<bool>,
    /// Some padding bit is 0, so they have to be stored.
    zero_padding: bool,
}

/// The contents of a `jbrd` box for the JPEG `d`, whose image is `p` and
/// whose metadata goes into the file as `meta` says. `None` when the file
/// cannot be rebuilt from one.
pub(super) fn data(d: &[u8], p: &Parsed, meta: &Meta) -> Option<Vec<u8>> {
    // What a decoder accepts as coefficients of a JPEG.
    for c in &p.comps {
        for b in &c.blocks {
            if !(-2047..=2047).contains(&b[0]) || b[1..].iter().any(|v| !(-4095..=4095).contains(v))
            {
                return None;
            }
        }
    }
    let mut r = parse(d, p)?;
    mark_apps(&mut r, meta);
    rebuilds(&r, d, p).then(|| write(&r, p))
}

// ---------------------------------------------------------------- reading

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

/// Huffman codes by destination, for decoding (`Lut`) or coding (`Code`).
struct Tables<T> {
    dc: [Option<T>; 4],
    ac: [Option<T>; 4],
}

impl<T> Default for Tables<T> {
    fn default() -> Self {
        Tables {
            dc: [None, None, None, None],
            ac: [None, None, None, None],
        }
    }
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some((*d.get(at)? as usize) << 8 | *d.get(at + 1)? as usize)
}

/// Reads the file's structure, tracing its entropy-coded data.
fn parse<'a>(d: &'a [u8], p: &Parsed) -> Option<Recon<'a>> {
    if !d.starts_with(&[0xFF, 0xD8]) || p.comps.len() != 3 || p.ids.len() != 3 {
        return None;
    }
    let mut r = Recon {
        markers: Vec::new(),
        apps: Vec::new(),
        coms: Vec::new(),
        quant: Vec::new(),
        comp_quant: Vec::new(),
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

/// The blocks of a scan in coding order.
struct ScanBlocks<'p> {
    p: &'p Parsed,
    comps: &'p [ScanComp],
    interleaved: bool,
    mcus_x: usize,
    mcus_y: usize,
}

impl<'p> ScanBlocks<'p> {
    fn new(p: &'p Parsed, comps: &'p [ScanComp]) -> Option<Self> {
        let maxh = p.comps.iter().map(|c| c.h).max()?;
        let maxv = p.comps.iter().map(|c| c.v).max()?;
        let interleaved = comps.len() > 1;
        let first = &p.comps[comps[0].comp];
        let (gh, gv) = if interleaved {
            (1, 1)
        } else {
            (first.h, first.v)
        };
        let s = ScanBlocks {
            p,
            comps,
            interleaved,
            mcus_x: (p.width as usize * gh).div_ceil(8 * maxh),
            mcus_y: (p.height as usize * gv).div_ceil(8 * maxv),
        };
        // Every block the scan visits must be one the image holds.
        for sc in comps {
            let c = &p.comps[sc.comp];
            let (nx, ny) = s.per_mcu(sc);
            if s.mcus_x == 0
                || s.mcus_y == 0
                || s.mcus_x * nx > c.bw
                || (s.mcus_y * ny - 1) * c.bw + s.mcus_x * nx > c.blocks.len()
            {
                return None;
            }
        }
        Some(s)
    }

    /// Blocks of a component in each MCU, across and down.
    fn per_mcu(&self, sc: &ScanComp) -> (usize, usize) {
        let c = &self.p.comps[sc.comp];
        if self.interleaved { (c.h, c.v) } else { (1, 1) }
    }

    /// Calls `f` for each block of MCU (`mx`, `my`), with the component's
    /// place in the scan and the block's index in the component.
    fn each(&self, mx: usize, my: usize, f: &mut dyn FnMut(usize, usize)) {
        for (i, sc) in self.comps.iter().enumerate() {
            let (nx, ny) = self.per_mcu(sc);
            for iy in 0..ny {
                for ix in 0..nx {
                    f(i, (my * ny + iy) * self.p.comps[sc.comp].bw + mx * nx + ix);
                }
            }
        }
    }
}

/// The scan parameters as they apply: a sequential frame codes the whole
/// block at full precision whatever its scan header says.
fn band(progressive: bool, s: &Scan) -> (usize, usize, u32, u32) {
    if progressive {
        (s.ss as usize, s.se as usize, s.al as u32, s.ah as u32)
    } else {
        (0, 63, 0, 0)
    }
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

/// Marks the APP segments a decoder fills in from elsewhere in the file:
/// the color profile's chunks, and the Exif and XMP that are in boxes.
fn mark_apps(r: &mut Recon, meta: &Meta) {
    if let Some(icc) = &meta.icc {
        // The chunks as the file has them, in its order: numbered from 1,
        // each saying how many there are, and together the profile.
        let chunks: Vec<usize> = (0..r.apps.len())
            .filter(|&i| {
                let a = r.apps[i].0;
                a[0] == 0xE2 && a.len() > 17 && a[3..].starts_with(b"ICC_PROFILE\0")
            })
            .collect();
        let whole = chunks.len() < 256
            && chunks.iter().enumerate().all(|(n, &i)| {
                let a = r.apps[i].0;
                a[15] as usize == n + 1 && a[16] as usize == chunks.len()
            })
            && chunks
                .iter()
                .flat_map(|&i| r.apps[i].0[17..].iter())
                .eq(icc.iter());
        if whole {
            for i in chunks {
                r.apps[i].1 = App::Icc;
            }
        }
    }
    for (kind, tag, body) in [
        (App::Exif, &b"Exif\0\0"[..], &meta.exif),
        (App::Xmp, &b"http://ns.adobe.com/xap/1.0/\0"[..], &meta.xmp),
    ] {
        let Some(body) = body else { continue };
        let found = r.apps.iter().position(|(a, k)| {
            *k == App::Unknown
                && a[0] == 0xE1
                && (a.get(3..).and_then(|x| x.strip_prefix(tag))).is_some_and(|x| x == &body[..])
        });
        if let Some(i) = found {
            r.apps[i].1 = kind;
        }
    }
}

// ---------------------------------------------------------------- rebuilding

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
struct Out<'a> {
    d: &'a [u8],
    pos: usize,
    same: bool,
    /// Entropy-coded bits not yet making a byte.
    acc: u64,
    n: u32,
    /// Tests keep the bytes instead, to make files from changed data.
    #[cfg(test)]
    kept: Option<Vec<u8>>,
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
fn rebuilds(r: &Recon, d: &[u8], p: &Parsed) -> bool {
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

fn rebuild(r: &Recon, p: &Parsed, out: &mut Out) -> bool {
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
                let mut s = vec![0xFF, m, 0, 17, 8];
                s.extend_from_slice(&p.height.to_be_bytes());
                s.extend_from_slice(&p.width.to_be_bytes());
                s.push(3);
                for (i, c) in p.comps.iter().enumerate() {
                    s.extend_from_slice(&[
                        p.ids[i],
                        ((c.h << 4) | c.v) as u8,
                        r.quant[r.comp_quant[i]].index,
                    ]);
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
                    let used = |j: usize| (0..3).find(|&c| r.comp_quant[c] == j);
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

// ---------------------------------------------------------------- writing

/// Distributions of the box's variable-length numbers: (offset, bits).
type Dist = [(u32, u32); 4];
const ONE_TO_FOUR: Dist = [(1, 0), (2, 0), (3, 0), (4, 0)];
const COUNT: Dist = [(0, 0), (1, 2), (4, 4), (20, 16)];
const BLOCK_STEP: Dist = [(0, 0), (1, 3), (9, 5), (41, 28)];

/// The box: the fields, then the bytes kept as they are (APP segments
/// not held elsewhere, comments, bytes between markers, the tail) as a
/// Brotli stream.
fn write(r: &Recon, p: &Parsed) -> Vec<u8> {
    let mut w = Bits::default();
    w.bit(false); // not grayscale
    for &m in &r.markers {
        w.put(6, (m - 0xC0) as u64);
    }
    for &(app, kind) in &r.apps {
        w.u32(kind as u32, [(0, 0), (1, 0), (2, 1), (4, 2)]);
        w.put(16, (app.len() - 1) as u64);
    }
    for com in &r.coms {
        w.put(16, (com.len() - 1) as u64);
    }
    w.u32(r.quant.len() as u32, ONE_TO_FOUR);
    for q in &r.quant {
        w.bit(q.wide);
        w.put(2, q.index as u64);
        w.bit(q.last);
    }
    match p.ids[..] {
        [1, 2, 3] => w.put(2, 1),
        [b'R', b'G', b'B'] => w.put(2, 2),
        _ => {
            w.put(2, 3);
            w.u32(3, ONE_TO_FOUR);
            for &id in &p.ids {
                w.put(8, id as u64);
            }
        }
    }
    for &q in &r.comp_quant {
        w.put(2, q as u64);
    }
    w.u32(r.huff.len() as u32, [(4, 0), (2, 3), (10, 4), (26, 6)]);
    for h in &r.huff {
        w.bit(h.slot & 0x10 != 0);
        w.put(2, (h.slot & 3) as u64);
        w.bit(h.last);
        // One more code of the longest length stands for the code of all
        // ones, which no symbol has, and ends the symbols as 256.
        let mut counts = h.counts.map(|c| c as u32);
        if !h.empty {
            counts[(1..=16).rev().find(|&l| counts[l] != 0).unwrap_or(1)] += 1;
        }
        for c in counts {
            w.u32(c, [(0, 0), (1, 0), (2, 3), (0, 8)]);
        }
        if !h.empty {
            for v in h.values.iter().map(|&v| v as u32).chain([256]) {
                w.u32(v, [(0, 2), (4, 2), (8, 4), (1, 8)]);
            }
        }
    }
    for s in &r.scans {
        w.u32(s.comps.len() as u32, ONE_TO_FOUR);
        w.put(6, s.ss as u64);
        w.put(6, s.se as u64);
        w.put(4, s.al as u64);
        w.put(4, s.ah as u64);
        for c in &s.comps {
            w.put(2, c.comp as u64);
            w.put(2, c.ac as u64);
            w.put(2, c.dc as u64);
        }
        w.u32(0, [(0, 0), (1, 0), (2, 0), (3, 3)]); // last pass needed
    }
    if let Some(v) = r.restart {
        w.put(16, v as u64);
    }
    for s in &r.scans {
        w.u32(s.resets.len() as u32, COUNT);
        let mut next = 0;
        for &b in &s.resets {
            w.u32(b - next, BLOCK_STEP);
            next = b + 1;
        }
        w.u32(s.zero_runs.len() as u32, COUNT);
        let mut next = 0;
        for &(b, n) in &s.zero_runs {
            w.u32(n, [(1, 0), (2, 2), (5, 4), (20, 8)]);
            w.u32(b - next, BLOCK_STEP);
            next = b + 1;
        }
    }
    for i in &r.inter {
        w.put(16, i.len() as u64);
    }
    w.u32(
        r.tail.len() as u32,
        [(0, 0), (1, 8), (257, 16), (65793, 22)],
    );
    w.bit(r.zero_padding);
    if r.zero_padding {
        w.put(24, r.padding.len() as u64);
        for &b in &r.padding {
            w.bit(b);
        }
    }
    w.pad();

    let raw: Vec<&[u8]> = (r.apps.iter())
        .filter(|a| a.1 == App::Unknown)
        .map(|a| a.0)
        .chain(r.coms.iter().copied())
        .chain(r.inter.iter().copied())
        .chain([r.tail])
        .collect();
    let mut out = w.out;
    brotli_stored(&raw.concat(), &mut out);
    out
}

/// `data` as a Brotli stream of uncompressed blocks. What goes here has
/// little to gain from compression: short segments, or data compressed
/// already.
fn brotli_stored(data: &[u8], out: &mut Vec<u8>) {
    let mut w = Bits::default();
    w.put(1, 0); // window of 2^16
    for chunk in data.chunks(1 << 24) {
        w.bit(false); // not the last block
        let len = chunk.len() as u64 - 1;
        let nibbles = if len < 1 << 16 {
            4
        } else if len < 1 << 20 {
            5
        } else {
            6
        };
        w.put(2, nibbles - 4);
        w.put(4 * nibbles as u32, len);
        w.bit(true); // uncompressed
        w.pad();
        w.out.extend_from_slice(chunk);
    }
    w.bit(true); // the last block,
    w.bit(true); // which is empty
    w.pad();
    out.extend_from_slice(&w.out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimize::{Limits, parse_and_decode};
    use crate::{Encoder, Sampling};

    fn image(w: u32, h: u32) -> Vec<u8> {
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let n = (s >> 59) as u8;
                // Flat panels leave long runs of zeros and of empty blocks.
                if (x / 24 + y / 16) % 2 == 0 {
                    px.extend_from_slice(&[200, 180, 40]);
                } else {
                    px.extend_from_slice(&[(x * 3) as u8 ^ n, (y * 5) as u8, n * 7]);
                }
            }
        }
        px
    }

    fn parsed(d: &[u8]) -> Parsed {
        parse_and_decode(d, false, &Limits::default()).unwrap()
    }

    fn boxed(d: &[u8]) -> Option<Vec<u8>> {
        let p = parsed(d);
        data(d, &p, &Meta::read(d, &p))
    }

    fn files() -> Vec<Vec<u8>> {
        let (w, h) = (150, 91);
        let rgb = image(w, h);
        let mut v = Vec::new();
        for sampling in [Sampling::S420, Sampling::S444] {
            for progressive in [false, true] {
                let e = Encoder::new(85).sampling(sampling).progressive(progressive);
                v.push(e.encode_rgb(&rgb, w, h));
            }
        }
        v
    }

    #[test]
    fn the_encoder_s_files_can_be_rebuilt() {
        for d in files() {
            assert!(boxed(&d).is_some());
        }
        // With a profile, Exif and data after the end.
        let rgb = image(40, 24);
        let mut icc = vec![0u8; 200];
        icc[..4].copy_from_slice(&200u32.to_be_bytes());
        icc[36..40].copy_from_slice(b"acsp");
        let mut d = Encoder::new(70)
            .icc_profile(&icc)
            .exif(b"II*\0\x08\0\0\0\0\0\0\0\0\0")
            .encode_rgb(&rgb, 40, 24);
        d.extend_from_slice(b"after the end");
        let p = parsed(&d);
        let mut r = parse(&d, &p).unwrap();
        mark_apps(&mut r, &Meta::read(&d, &p));
        let kinds: Vec<u8> = r.apps.iter().map(|a| a.1 as u8).collect();
        assert!(kinds.contains(&(App::Icc as u8)) && kinds.contains(&(App::Exif as u8)));
        assert_eq!(r.tail, b"after the end");
        assert!(rebuilds(&r, &d, &p));
    }

    /// A code with 255 symbols of its longest length is one the box's
    /// fields cannot hold, used or not.
    #[test]
    fn a_huffman_code_too_wide_for_the_box_is_refused() {
        let mut d = files().swap_remove(0);
        assert!(boxed(&d).is_some());
        let mut dht = vec![0xFF, 0xC4, 0x01, 0x12, 0x13];
        dht.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        dht.extend(0..255u8);
        let sos = d.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        d.splice(sos..sos, dht);
        assert!(crate::jpeg_to_jxl(&d).is_some());
        assert!(boxed(&d).is_none());
    }

    #[test]
    fn the_box_is_there_unless_turned_off() {
        let has_box = |d: &[u8]| d.windows(4).any(|w| w == b"jbrd");
        let d = &files()[0];
        let with = crate::jpeg_to_jxl(d).unwrap();
        let options = crate::JxlOptions::default().reconstruction(false);
        let without = crate::jpeg_to_jxl_with(d, &options).unwrap();
        assert!(has_box(&with) && !has_box(&without));
        assert!(without.len() < with.len());
        // Without it, and with no Exif or XMP, the codestream stands alone.
        assert!(without.starts_with(&[0xFF, 0x0A]));
    }

    #[test]
    fn a_changed_file_is_not_passed_as_rebuilt() {
        let d = &files()[0];
        let p = parsed(d);
        let mut r = parse(d, &p).unwrap();
        assert!(rebuilds(&r, d, &p));
        r.huff[0].values.swap(0, 1);
        assert!(!rebuilds(&r, d, &p));
    }

    /// Files a plain encoder does not write: runs of sixteen zeros before
    /// an end of block, end-of-band runs cut short, padding of zeros.
    #[test]
    fn unusual_entropy_coding_is_kept() {
        for (n, d) in files().iter().enumerate() {
            let p = parsed(d);
            let progressive = d.windows(2).any(|w| w == [0xFF, 0xC2]);
            let mut r = parse(d, &p).unwrap();
            assert!(r.scans.iter().all(|s| s.zero_runs.is_empty()));
            // The file's codes are made for its symbols alone: give the
            // AC ones every symbol, so that others can be written.
            for h in r.huff.iter_mut().filter(|h| h.slot & 0x10 != 0) {
                h.counts = [0; 17];
                (h.counts[9], h.counts[10]) = (255, 1);
                h.values = (0..=255).collect();
            }
            let mut want = Vec::new();
            for s in &mut r.scans {
                let (ss, se, _, ah) = band(progressive, s);
                if ah != 0 || se < 20 {
                    want.push(Vec::new());
                    continue;
                }
                // Blocks whose band ends in at least 16 zeros.
                let blocks = ScanBlocks::new(&p, &s.comps).unwrap();
                let (mut index, mut runs) = (0u32, Vec::new());
                for y in 0..blocks.mcus_y {
                    for x in 0..blocks.mcus_x {
                        blocks.each(x, y, &mut |i, bi| {
                            let blk = &p.comps[s.comps[i].comp].blocks[bi];
                            let al = if progressive { s.al } else { 0 };
                            let last = (ss.max(1)..=se)
                                .rev()
                                .find(|&k| blk[k].unsigned_abs() >> al != 0);
                            let zeros = (se - last.unwrap_or(ss.max(1) - 1)) as u32;
                            if zeros >= 16 && index % 3 == 0 {
                                runs.push((index, (zeros / 16).min(1 + index % 4)));
                            }
                            index += 1;
                        });
                    }
                }
                if progressive {
                    s.resets = (0..index).filter(|i| i % 5 == 2).collect();
                }
                s.zero_runs = runs.clone();
                want.push(runs);
            }
            assert!(want.iter().any(|w| !w.is_empty()), "file {n}");
            r.zero_padding = true;
            r.padding = vec![false; 4096];
            let mut d2 = written(&r, &p);
            assert_ne!(&d2, d);
            let p2 = parsed(&d2);
            let r2 = parse(&d2, &p2).unwrap();
            let got: Vec<Vec<(u32, u32)>> = r2.scans.iter().map(|s| s.zero_runs.clone()).collect();
            assert_eq!(got, want, "file {n}");
            assert!(r2.zero_padding || r2.padding.is_empty());
            if progressive {
                assert!(r2.scans.iter().any(|s| !s.resets.is_empty()), "file {n}");
            }
            assert!(boxed(&d2).is_some(), "file {n}");
            // And bytes between markers.
            d2.splice(2..2, [0, 1, 2]);
            assert!(boxed(&d2).is_some(), "file {n}");
        }
    }

    /// The file `r` describes, written as a decoder would (but with more
    /// padding on hand than the file takes).
    fn written(r: &Recon, p: &Parsed) -> Vec<u8> {
        let mut out = Out {
            d: &[],
            pos: 0,
            same: true,
            acc: 0,
            n: 0,
            kept: Some(Vec::new()),
        };
        rebuild(r, p, &mut out);
        assert!(out.same);
        out.kept.unwrap()
    }
}
