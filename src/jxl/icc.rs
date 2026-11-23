//! The color profile in the codestream.

use super::bits::Bits;
use super::entropy::{Tokens, write_histograms};

/// The ICC profile as JPEG XL codes it: a byte stream (here the profile
/// unpredicted: its header relative to the expected one, the rest
/// copied), entropy coded with contexts from the previous two bytes.
pub(super) fn write_icc(w: &mut Bits, icc: &[u8]) {
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
    let code = write_histograms(w, 41, tokens.iter().copied());
    code.put_all(w, tokens.iter().copied());
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
