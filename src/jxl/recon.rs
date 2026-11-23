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

use super::Meta;
use crate::optimize::Parsed;

mod read;
mod rebuild;
mod write;

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
    /// The sampling factors of each component as the frame header has
    /// them (horizontal in the high half).
    sampling: Vec<u8>,
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
    let mut r = read::parse(d, p)?;
    mark_apps(&mut r, meta);
    rebuild::rebuilds(&r, d, p).then(|| write::write(&r, p))
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

#[cfg(test)]
mod tests {
    use super::read::parse;
    use super::rebuild::{Out, rebuild, rebuilds};
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
        // Grayscale: the green of the same image.
        let gray: Vec<u8> = rgb.chunks(3).map(|p| p[1]).collect();
        for progressive in [false, true] {
            v.push(
                Encoder::new(85)
                    .progressive(progressive)
                    .encode_gray(&gray, w, h),
            );
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
        assert!(!crate::jpeg_to_jxl(&d).unwrap().reconstruction);
        assert!(boxed(&d).is_none());
    }

    /// One component may give any sampling factors in its frame header;
    /// they change nothing in the image, but the file has them.
    #[test]
    fn grayscale_keeps_its_header_s_sampling_factors() {
        let gray: Vec<u8> = image(150, 91).chunks(3).map(|p| p[1]).collect();
        for progressive in [false, true] {
            let e = Encoder::new(85).progressive(progressive);
            let d = e.encode_gray(&gray, 150, 91);
            let sof = (d.windows(2))
                .position(|w| w[0] == 0xFF && matches!(w[1], 0xC0 | 0xC2))
                .unwrap();
            assert_eq!(d[sof + 11], 0x11);
            let plain = crate::jpeg_to_jxl(&d).unwrap();
            assert!(plain.reconstruction);
            for (hv, kept) in [
                (0x21, true),
                (0x12, true),
                (0x22, true),
                (0x41, false),
                (0x13, false),
            ] {
                let mut d = d.clone();
                d[sof + 11] = hv;
                let out = crate::jpeg_to_jxl(&d).unwrap();
                assert_eq!(out.reconstruction, kept, "{hv:#x}");
                // Without the data to rebuild it, the file is the plain one's.
                let options = crate::JxlOptions::default().reconstruction(false);
                let small = crate::jpeg_to_jxl_with(&d, &options).unwrap();
                assert!(!small.reconstruction);
                if !kept {
                    assert_eq!(out.data, small.data, "{hv:#x}");
                }
            }
        }
    }

    #[test]
    fn the_box_is_there_unless_turned_off() {
        let has_box = |d: &[u8]| d.windows(4).any(|w| w == b"jbrd");
        let d = &files()[0];
        let with = crate::jpeg_to_jxl(d).unwrap();
        let options = crate::JxlOptions::default().reconstruction(false);
        let without = crate::jpeg_to_jxl_with(d, &options).unwrap();
        assert!(with.reconstruction && has_box(&with.data));
        assert!(!without.reconstruction && !has_box(&without.data));
        assert!(without.data.len() < with.data.len());
        // Without it, and with no Exif or XMP, the codestream stands alone.
        assert!(without.data.starts_with(&[0xFF, 0x0A]));
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
