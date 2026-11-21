//! JPEG to JPEG XL recompression. Experimental: behind the `jxl`
//! feature, and outside the crate's compatibility promise until it
//! settles; the functions and their output may change in any release.
//!
//! The JPEG's quantized DCT coefficients are stored as they are in a
//! JPEG XL VarDCT frame (8x8 DCT blocks, the JPEG's own quantization
//! tables, YCbCr and the JPEG's chroma subsampling; a grayscale image is
//! luma with empty chroma), so the image decodes
//! to what the JPEG holds, with no generation loss. Only the entropy
//! coding changes. Reconstruction data (`recon`) lets a decoder write the
//! JPEG file itself back.
//!
//! Written from the JPEG XL format (ISO/IEC 18181-1).

use crate::optimize::{Limits, Parsed, parse_and_decode};

mod bits;
mod entropy;
mod frame;
mod icc;
mod modular;
mod recon;

use frame::Frame;

/// Recompress a baseline or progressive JPEG, YCbCr or grayscale, as
/// JPEG XL without changing its coefficients. `None` for files this
/// cannot carry over (CMYK, RGB-coded JPEGs, sampling factors other than
/// 1 and 2, a zero in a quantization table).
///
/// The result also holds what a JPEG XL decoder needs to write the
/// original file back, byte for byte: see
/// [`JxlOptions::reconstruction`] for what that costs and when it is
/// left out. The color profile, Exif and XMP carry over either way
/// ([`strip_metadata`](crate::strip_metadata) first removes them).
///
/// It runs on one thread whatever the features, and takes about 20 to 50
/// bytes of memory per pixel, more the finer the JPEG's quantization: a
/// 24-megapixel photo needs about 400 MB. [`JxlOptions::limits`] bounds
/// the frame it accepts.
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
    /// byte for byte. Default true. Without it the image is the same, but
    /// the file it came from is gone for good.
    ///
    /// For most files it is a few hundred bytes. It also holds, as they
    /// are, the parts of the file that are neither image nor color
    /// profile, Exif or XMP: other APP segments, comments, and data after
    /// the end of the image, so a file with a large trailer grows by
    /// about that much.
    ///
    /// It is left out, with no sign in the return value, when the format
    /// cannot describe the file: one that changes its restart interval
    /// between scans, for example, or has more than about 4 MB after the
    /// end of the image. The image is carried over all the same. Output
    /// with it is a JPEG XL container holding a `jbrd` box.
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
                    // A profile for other than one channel says nothing
                    // about a grayscale image.
                    && (p.comps.len() != 1 || &icc[16..20] == b"GRAY")
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

#[cfg(test)]
mod tests {
    use super::bits::Bits;
    use super::entropy::write_ans_histogram;
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
    fn grayscale_keeps_only_a_grayscale_profile() {
        let gray = [100u8; 16 * 16];
        let profile = |space: &[u8; 4]| {
            let mut icc = vec![0u8; 200];
            icc[..4].copy_from_slice(&200u32.to_be_bytes());
            icc[16..20].copy_from_slice(space);
            icc[36..40].copy_from_slice(b"acsp");
            icc
        };
        for (space, kept) in [(b"GRAY", true), (b"RGB ", false)] {
            let e = crate::Encoder::new(80).icc_profile(&profile(space));
            let jpeg = e.encode_gray(&gray, 16, 16);
            let p = parse_and_decode(&jpeg, false, &Limits::default()).unwrap();
            assert_eq!(Meta::read(&jpeg, &p).icc.is_some(), kept);
            assert!(jpeg_to_jxl(&jpeg).is_some());
        }
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
