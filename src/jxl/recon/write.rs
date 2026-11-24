//! The reconstruction data as the bytes of a `jbrd` box.

use super::{App, Recon};
use crate::jxl::bits::Bits;
use crate::optimize::Parsed;

/// Distributions of the box's variable-length numbers: (offset, bits).
type Dist = [(u32, u32); 4];
const ONE_TO_FOUR: Dist = [(1, 0), (2, 0), (3, 0), (4, 0)];
const COUNT: Dist = [(0, 0), (1, 2), (4, 4), (20, 16)];
const BLOCK_STEP: Dist = [(0, 0), (1, 3), (9, 5), (41, 28)];

/// The box: the fields, then the bytes kept as they are (APP segments
/// not held elsewhere, comments, bytes between markers, the tail) as a
/// Brotli stream.
pub(super) fn write(r: &Recon, p: &Parsed) -> Vec<u8> {
    let mut w = Bits::default();
    w.bit(p.comps.len() == 1); // grayscale
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
        [1] => w.put(2, 0),
        [1, 2, 3] => w.put(2, 1),
        [b'R', b'G', b'B'] => w.put(2, 2),
        _ => {
            w.put(2, 3);
            w.u32(p.ids.len() as u32, ONE_TO_FOUR);
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
    brotli_stored(&raw, &mut out);
    out
}

/// `parts`, one after another, as a Brotli stream of uncompressed blocks.
/// What goes here has little to gain from compression: short segments, or
/// data compressed already. It can be most of a file (comments, data
/// after the end), so it is copied once, into room made for it.
fn brotli_stored(parts: &[&[u8]], out: &mut Vec<u8>) {
    const BLOCK: usize = 1 << 24;
    let mut left: usize = parts.iter().map(|p| p.len()).sum();
    // A block's header is at most 29 bits; the first has one bit before
    // it, and two more end the stream.
    out.reserve(left + 4 * left.div_ceil(BLOCK) + 2);
    let mut parts = parts.iter().copied();
    let mut part: &[u8] = &[];
    let mut w = Bits::default();
    w.put(1, 0); // window of 2^16
    while left > 0 {
        let n = left.min(BLOCK);
        w.bit(false); // not the last block
        let len = n as u64 - 1;
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
        out.append(&mut w.out);
        let mut need = n;
        while need > 0 {
            if part.is_empty() {
                part = parts.next().expect("the parts hold what was counted");
            }
            let k = need.min(part.len());
            out.extend_from_slice(&part[..k]);
            part = &part[k..];
            need -= k;
        }
        left -= n;
    }
    w.bit(true); // the last block,
    w.bit(true); // which is empty
    w.pad();
    out.extend_from_slice(&w.out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream is that of the parts joined, whatever their lengths and
    /// wherever the 16 MB blocks fall.
    #[test]
    fn stored_stream_of_parts() {
        // The stream written from one slice, a block at a time.
        fn joined(data: &[u8]) -> Vec<u8> {
            let mut w = Bits::default();
            w.put(1, 0);
            for chunk in data.chunks(1 << 24) {
                w.bit(false);
                let len = chunk.len() as u64 - 1;
                let nibbles = 4 + (len >= 1 << 16) as u64 + (len >= 1 << 20) as u64;
                w.put(2, nibbles - 4);
                w.put(4 * nibbles as u32, len);
                w.bit(true);
                w.pad();
                w.out.extend_from_slice(chunk);
            }
            w.bit(true);
            w.bit(true);
            w.pad();
            w.out
        }
        let big: Vec<u8> = (0..(1usize << 24) + 70_000)
            .map(|i| ((i * 31) >> 3) as u8)
            .collect();
        let cases: [&[&[u8]]; 6] = [
            &[],
            &[&[]],
            &[b"a"],
            &[b"one", &[], b"two", &big[..65_536], &big[..65_537]],
            &[
                &big[..(1 << 24) - 2],
                b"abc",
                &big[..1 << 20],
                &big[5..(1 << 20) + 9],
            ],
            &[&big, &big[..(1 << 24) - 70_000]],
        ];
        for parts in cases {
            let mut out = vec![7, 7];
            brotli_stored(parts, &mut out);
            assert_eq!(out[..2], [7, 7]);
            assert!(
                out[2..] == joined(&parts.concat())[..],
                "{} parts",
                parts.len()
            );
        }
    }
}
