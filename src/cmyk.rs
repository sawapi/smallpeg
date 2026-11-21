//! Four-component (CMYK / YCCK) JPEG: decode to the stored channel values
//! and encode them back, without ever interpreting them as color.
//!
//! CMYK JPEGs come in two codings, told apart by the Adobe APP14 segment's
//! transform byte: plain CMYK (transform 0, or no APP14 at all) and YCCK
//! (transform 2), where the first three channels went through the RGB to
//! YCbCr transform on the way in (as `255 - C` etc.) and K is coded as is.
//! On top of that, files written by Adobe software conventionally store
//! *inverted* ink values (0 = full ink), and readers infer this from the
//! APP14's presence. No conversion to RGB is right for every file.
//!
//! So nothing here converts. [`decode_cmyk`] returns the CMYK values the
//! file stores (for YCCK, the values after undoing the transform — exactly
//! what mainstream decoders return as CMYK output), plus which APP14
//! coding the file used. [`resize_cmyk`](crate::resize_cmyk) is linear per
//! channel, so inverted values stay consistently inverted.
//! [`Encoder::encode_cmyk`] writes the values back and writes its *own*
//! APP14 whose transform byte matches the coding it actually performed, so
//! the two can never disagree. Passing the decoded [`CmykImage::adobe`]
//! back keeps the inversion convention readers will assume unchanged.

use crate::component::{Component, interleave_order};
use crate::dct::dequant_scale;
use crate::decode::{Upsampler, sample_planes, ycc_row};
use crate::dering::Dering;
use crate::document::encode_document;
use crate::optimize::{Limits, parse_and_decode};
use crate::quant::scaled_quant_table;
use crate::{
    Encoder, HF_BLEND_MAX, QuantPreset, Sampling, ceil_div, compress_luma_table, is_gradient,
};

/// Components deringed by default (bit i = component i): none. Measured
/// on CMYK images from print-oriented PDFs (photos, line art, banners on
/// white): Y+K or K alone was neutral (within 1% at equal SSIMULACRA2 and
/// butteraugli) at 25% more encode time. Those sources are themselves
/// JPEGs, so their paper is near 0 but rarely exactly at the clip point,
/// which is what deringing needs. `Encoder::cmyk_dering` turns it on.
const CMYK_DERING_DEFAULT: u8 = 0;

/// How a four-component JPEG's channels are coded, from the Adobe APP14
/// segment's transform byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdobeTransform {
    /// Transform 0: the four channels are stored as CMYK directly.
    Cmyk,
    /// Transform 2: C, M and Y are stored through the YCbCr transform
    /// (applied to `255 - C`, `255 - M`, `255 - Y`), K directly. This is
    /// the usual choice of print-oriented tools, and it allows chroma
    /// subsampling.
    Ycck,
}

/// A decoded four-component JPEG; see [`decode_cmyk`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmykImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Interleaved C, M, Y, K bytes, `width * height * 4`, exactly as the
    /// file stores them: not inverted, not color-managed.
    pub pixels: Vec<u8>,
    /// The file's Adobe APP14 coding, `None` if it has no APP14 segment.
    /// Hand this to [`Encoder::encode_cmyk`] to write the same convention
    /// back.
    pub adobe: Option<AdobeTransform>,
}

/// Decode a four-component (CMYK or YCCK) JPEG to its stored channel
/// values, without color conversion.
///
/// For YCCK files the YCbCr transform is undone, so the result is the
/// CMYK the encoder was given; whether those values are inverted is a
/// convention of the producer that this function deliberately does not
/// interpret. Subsampled components are interpolated linearly, as
/// mainstream decoders do by default.
///
/// Returns `None` for anything that is not a four-component baseline or
/// progressive Huffman-coded 8-bit JPEG, or that is malformed. Like
/// [`optimize_jpeg`](crate::optimize_jpeg), it takes untrusted input and
/// never panics. Uses the default [`Limits`] (100 megapixels, 128 scans);
/// [`decode_cmyk_with`] takes others.
pub fn decode_cmyk(jpeg: &[u8]) -> Option<CmykImage> {
    decode_cmyk_with(jpeg, &Limits::default())
}

/// [`decode_cmyk`] with caller-chosen [`Limits`]; `None` also when the
/// input exceeds them.
pub fn decode_cmyk_with(jpeg: &[u8], limits: &Limits) -> Option<CmykImage> {
    let p = parse_and_decode(jpeg, true, limits)?;
    if p.comps.len() != 4 {
        return None;
    }
    // The conventional rule for four components: APP14 transform 0 is CMYK, any
    // other value is YCCK; without an APP14 the file is plain CMYK.
    let adobe = p.adobe.map(|t| {
        if t == 0 {
            AdobeTransform::Cmyk
        } else {
            AdobeTransform::Ycck
        }
    });
    let (w, h) = (p.width as usize, p.height as usize);
    let planes = sample_planes(&p)?;
    let mut ups: Vec<Upsampler> = (0..4).map(|i| Upsampler::new(&planes[i], &p, i)).collect();

    let ycck = adobe == Some(AdobeTransform::Ycck);
    let mut pixels = vec![0u8; w * h * 4];
    let mut rgb = vec![[0u8; 3]; if ycck { w } else { 0 }];
    for (y, o) in pixels.chunks_exact_mut(w * 4).enumerate() {
        ups.iter_mut().for_each(|u| u.advance(y));
        let r: [&[u8]; 4] = core::array::from_fn(|i| ups[i].row());
        let o = o.as_chunks_mut::<4>().0;
        if ycck {
            // YCCK carries 255 - C etc. through the YCbCr transform.
            ycc_row(r[0], r[1], r[2], &mut rgb);
            for ((px, c), &k) in o.iter_mut().zip(&rgb).zip(r[3]) {
                *px = [255 - c[0], 255 - c[1], 255 - c[2], k];
            }
        } else {
            for (px, (((&c, &m), &yy), &k)) in
                o.iter_mut().zip(r[0].iter().zip(r[1]).zip(r[2]).zip(r[3]))
            {
                *px = [c, m, yy, k];
            }
        }
    }
    Some(CmykImage {
        width: w as u32,
        height: h as u32,
        pixels,
        adobe,
    })
}

/// One component's coefficients from a sample function over a `pw x ph`
/// plane, on a `gw x gh` block grid (padding replicates the edge).
#[allow(clippy::too_many_arguments)]
fn plane_component(
    sample: impl Fn(usize, usize) -> f32,
    pw: usize,
    ph: usize,
    gw: usize,
    gh: usize,
    hv: (u8, u8),
    qt_id: u8,
    recip: &[f32; 64],
    der: Option<&Dering>,
) -> Component {
    let mut c = Component::with_grid(
        gw,
        ceil_div(pw, 8),
        ceil_div(ph, 8),
        hv.0,
        hv.1,
        qt_id,
        gw * gh,
    );
    // The gradient probe may be any of Y or K; track all (cheap here).
    c.energy = true;
    let mut px = [0f32; 64];
    for by in 0..gh {
        for bx in 0..gw {
            for yy in 0..8 {
                let sy = (by * 8 + yy).min(ph - 1);
                for xx in 0..8 {
                    let sx = (bx * 8 + xx).min(pw - 1);
                    px[yy * 8 + xx] = sample(sx, sy) - 128.0;
                }
            }
            if let Some(d) = der {
                d.apply(&mut px);
            }
            c.push_block(&mut px, recip);
        }
    }
    c
}

impl Encoder {
    /// Encode interleaved 8-bit C, M, Y, K pixels without color conversion.
    ///
    /// `adobe` selects the coding and the APP14 segment written for it:
    ///
    /// - `None`: plain CMYK, no APP14.
    /// - `Some(AdobeTransform::Cmyk)`: plain CMYK, APP14 with transform 0.
    /// - `Some(AdobeTransform::Ycck)`: YCCK, APP14 with transform 2. C, M
    ///   and Y go through the YCbCr transform and the chroma pair follows
    ///   [`Encoder::sampling`]; K stays at full resolution. Usually the
    ///   smallest of the three.
    ///
    /// The APP14 is always generated here, never copied, so its transform
    /// byte matches the coding performed. Readers take the APP14's presence
    /// as a hint that the values are inverted, so to round-trip a file pass
    /// back the [`CmykImage::adobe`] that [`decode_cmyk`] returned. ICC and
    /// EXIF set on the encoder are embedded as usual; no JFIF segment is
    /// written (JFIF is defined for one and three components only).
    ///
    /// Plain CMYK channels all use the luma quantization table; YCCK uses
    /// luma for Y and K, chroma for Cb and Cr.
    ///
    /// # Panics
    ///
    /// Panics if `cmyk.len()` is not exactly `width * height * 4`, or if
    /// either dimension is 0 or above 65535 (the JPEG frame-header limit).
    pub fn encode_cmyk(
        &self,
        cmyk: &[u8],
        width: u32,
        height: u32,
        adobe: Option<AdobeTransform>,
    ) -> Vec<u8> {
        assert_eq!(cmyk.len(), width as usize * height as usize * 4);
        assert!(width > 0 && height > 0 && width <= 65535 && height <= 65535);
        let (w, h) = (width as usize, height as usize);
        let ycck = adobe == Some(AdobeTransform::Ycck);
        let (base_l, base_c) = self.base_tables();
        let qt_luma = scaled_quant_table(base_l, self.quality);
        let qt_chroma = scaled_quant_table(base_c, self.quality);

        let (h2, v2) = match (ycck, self.sampling) {
            (true, Sampling::S420) => (2usize, 2usize),
            _ => (1, 1),
        };
        let mcu_w = ceil_div(w, 8 * h2);
        let mcu_h = ceil_div(h, 8 * v2);
        let (gw, gh) = (mcu_w * h2, mcu_h * v2);
        let (cw, chh) = (ceil_div(w, h2), ceil_div(h, v2));

        // Full-resolution source planes: C, M, Y, K, or Y, Cb, Cr, K.
        let mut planes: Vec<Vec<u8>> = vec![vec![0u8; w * h]; 4];
        for (i, p) in cmyk.as_chunks::<4>().0.iter().enumerate() {
            if ycck {
                let (r, g, b) = (
                    255.0 - p[0] as f32,
                    255.0 - p[1] as f32,
                    255.0 - p[2] as f32,
                );
                let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
                planes[0][i] = q(0.299 * r + 0.587 * g + 0.114 * b);
                planes[1][i] = q(-0.168736 * r - 0.331264 * g + 0.5 * b + 128.0);
                planes[2][i] = q(0.5 * r - 0.418688 * g - 0.081312 * b + 128.0);
            } else {
                planes[0][i] = p[0];
                planes[1][i] = p[1];
                planes[2][i] = p[2];
            }
            planes[3][i] = p[3];
        }
        // Box-averaged chroma for 4:2:0 YCCK.
        let chroma: Option<[Vec<f32>; 2]> = (h2 == 2).then(|| {
            core::array::from_fn(|k| {
                let src = &planes[1 + k];
                let mut out = vec![0f32; cw * chh];
                for cy in 0..chh {
                    let (y0, y1) = ((cy * 2).min(h - 1), (cy * 2 + 1).min(h - 1));
                    for cx in 0..cw {
                        let (x0, x1) = ((cx * 2).min(w - 1), (cx * 2 + 1).min(w - 1));
                        let s = src[y0 * w + x0] as f32
                            + src[y0 * w + x1] as f32
                            + src[y1 * w + x0] as f32
                            + src[y1 * w + x1] as f32;
                        out[cy * cw + cx] = s * 0.25;
                    }
                }
                out
            })
        });

        let auto_hf = self.table_tweak == (1.0, 0.0) && self.quant == QuantPreset::Photo;
        let beta_q = HF_BLEND_MAX * ((self.quality as f32 - 60.0) / 30.0).clamp(0.0, 1.0);
        let full = |i: usize| {
            let p = &planes[i];
            move |x: usize, y: usize| p[y * w + x] as f32
        };
        // Clipped-pixel deringing on the channels that carry the page:
        // which end is paper depends on the (uninterpreted) inversion
        // convention, so both ends are free.
        let mask = self.cmyk_dering.unwrap_or(CMYK_DERING_DEFAULT);
        let build = |qt_l: &[u16; 64]| -> Vec<Component> {
            let der = self.make_dering(qt_l).map(Dering::both_ends);
            let der_for = |i: usize| der.as_ref().filter(|_| mask >> i & 1 == 1);
            let recip_l = dequant_scale(qt_l);
            let recip_c = dequant_scale(&qt_chroma);
            let hv = (h2 as u8, v2 as u8);
            let mut comps = Vec::with_capacity(4);
            if ycck {
                comps.push(plane_component(
                    full(0),
                    w,
                    h,
                    gw,
                    gh,
                    hv,
                    0,
                    &recip_l,
                    der_for(0),
                ));
                for k in 0..2 {
                    let c = match &chroma {
                        Some(ch) => {
                            let p = &ch[k];
                            plane_component(
                                |x, y| p[y * cw + x],
                                cw,
                                chh,
                                mcu_w,
                                mcu_h,
                                (1, 1),
                                1,
                                &recip_c,
                                der_for(1 + k),
                            )
                        }
                        None => plane_component(
                            full(1 + k),
                            w,
                            h,
                            gw,
                            gh,
                            hv,
                            1,
                            &recip_c,
                            der_for(1 + k),
                        ),
                    };
                    comps.push(c);
                }
            } else {
                for i in 0..3 {
                    comps.push(plane_component(
                        full(i),
                        w,
                        h,
                        gw,
                        gh,
                        hv,
                        0,
                        &recip_l,
                        der_for(i),
                    ));
                }
            }
            comps.push(plane_component(
                full(3),
                w,
                h,
                gw,
                gh,
                hv,
                0,
                &recip_l,
                der_for(3),
            ));
            comps
        };

        // The quality-adaptive luma table shape, gated on the
        // luminance-like channel: Y for YCCK, K for plain CMYK.
        let probe = if ycck { 0 } else { 3 };
        let mut qt_used = if auto_hf && beta_q > 0.0 {
            compress_luma_table(&qt_luma, beta_q)
        } else {
            qt_luma
        };
        let mut comps = build(&qt_used);
        if auto_hf && beta_q > 0.0 && is_gradient(&comps[probe]) {
            qt_used = qt_luma;
            comps = build(&qt_used);
        }
        drop(planes);

        let mut markers = self.app_markers();
        if let Some(t) = adobe {
            // "Adobe", version 100, flags0 = flags1 = 0, transform.
            markers.extend_from_slice(&[0xFF, 0xEE, 0x00, 0x0E]);
            markers.extend_from_slice(b"Adobe");
            markers.extend_from_slice(&[0x00, 0x64, 0x00, 0x00, 0x00, 0x00]);
            markers.push(match t {
                AdobeTransform::Cmyk => 0,
                AdobeTransform::Ycck => 2,
            });
        }
        let qts: &[[u16; 64]] = if ycck {
            &[qt_used, qt_chroma]
        } else {
            &[qt_used]
        };
        let order = interleave_order(&comps, mcu_w, mcu_h);
        encode_document(
            comps,
            &order,
            qts,
            width as u16,
            height as u16,
            &markers,
            self.lambda.unwrap_or(crate::DEFAULT_LAMBDA),
            self.chroma_scale(),
            0.0,
            self.progressive,
            self.lambda.is_none(),
            self.deadzone,
            self.aq_lite,
            false,
            self.threads,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cmyk(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                v.push((x * 255 / w) as u8);
                v.push((y * 255 / h) as u8);
                v.push(((x + y) * 3 % 256) as u8);
                // K: a text-like hard-edged pattern on a flat background.
                v.push(if (x / 4 + y / 6) % 3 == 0 { 30 } else { 230 });
            }
        }
        v
    }

    fn max_diff(a: &[u8], b: &[u8]) -> (u8, f64) {
        let mut m = 0u8;
        let mut s = 0f64;
        for (&x, &y) in a.iter().zip(b) {
            let d = x.abs_diff(y);
            m = m.max(d);
            s += d as f64;
        }
        (m, s / a.len() as f64)
    }

    fn app14_transform(j: &[u8]) -> Option<u8> {
        let mut pos = 2;
        while pos + 4 <= j.len() && j[pos] == 0xFF {
            let m = j[pos + 1];
            let len = (j[pos + 2] as usize) << 8 | j[pos + 3] as usize;
            if m == 0xDA {
                break;
            }
            if m == 0xEE && j[pos + 4..].starts_with(b"Adobe") {
                return Some(j[pos + 15]);
            }
            pos += 2 + len;
        }
        None
    }

    #[test]
    fn round_trip_every_coding() {
        let (w, h) = (61u32, 45u32);
        let src = test_cmyk(w, h);
        for adobe in [None, Some(AdobeTransform::Cmyk), Some(AdobeTransform::Ycck)] {
            for s in [Sampling::S444, Sampling::S420] {
                for prog in [false, true] {
                    let j = Encoder::new(98)
                        .sampling(s)
                        .progressive(prog)
                        .encode_cmyk(&src, w, h, adobe);
                    assert_eq!(app14_transform(&j), adobe.map(|t| t as u8 * 2));
                    assert!(!j.windows(5).any(|x| x == b"JFIF\0"));
                    let d = decode_cmyk(&j).expect("own output decodes");
                    assert_eq!((d.width, d.height, d.adobe), (w, h, adobe));
                    let (m, mean) = max_diff(&d.pixels, &src);
                    let sub = adobe == Some(AdobeTransform::Ycck) && s == Sampling::S420;
                    // 4:2:0 YCCK loses chroma detail by design; the rest is
                    // near-lossless at q98.
                    let lim = if sub { 12.0 } else { 1.5 };
                    assert!(
                        mean < lim,
                        "{adobe:?} {s:?} prog={prog}: max {m} mean {mean:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn decoder_matches_independent_decoder() {
        // zune-jpeg renders CMYK as RGB via (C * K) / 255 per channel, with
        // YCCK first mapped to CMYK the conventional way. Rebuild that from our
        // stored values and compare. 4:4:4, since its chroma upsampling
        // interpolates where ours replicates.
        let (w, h) = (40u32, 33u32);
        let src = test_cmyk(w, h);
        for adobe in [Some(AdobeTransform::Cmyk), Some(AdobeTransform::Ycck)] {
            let j = Encoder::new(80)
                .sampling(Sampling::S444)
                .encode_cmyk(&src, w, h, adobe);
            let ours = decode_cmyk(&j).unwrap();
            let theirs = image::load_from_memory(&j).unwrap().to_rgb8().into_raw();
            let blinn = |a: u8, b: u8| -> u8 {
                let t = a as u32 * b as u32 + 128;
                ((t + (t >> 8)) >> 8) as u8
            };
            let mut ours_rgb = Vec::new();
            for p in ours.pixels.as_chunks::<4>().0 {
                for c in 0..3 {
                    ours_rgb.push(blinn(p[c], p[3]));
                }
            }
            let (m, mean) = max_diff(&ours_rgb, &theirs);
            assert!(m <= 3 && mean < 0.5, "{adobe:?}: max {m} mean {mean:.3}");
        }
    }

    #[test]
    fn decode_resize_encode_keeps_inversion_and_coding() {
        let (w, h) = (64u32, 48u32);
        let src = test_cmyk(w, h);
        let enc = Encoder::new(90).sampling(Sampling::S444);
        let j = enc.encode_cmyk(&src, w, h, Some(AdobeTransform::Ycck));
        let d = decode_cmyk(&j).unwrap();
        let small = crate::resize_cmyk(&d.pixels, w, h, 32, 24);
        let j2 = enc.encode_cmyk(&small, 32, 24, d.adobe);
        assert_eq!(app14_transform(&j2), Some(2));
        let d2 = decode_cmyk(&j2).unwrap();
        // Same orientation of values as a direct downscale of the source
        // (an inversion slip would put the mean error near 100).
        let direct = crate::resize_cmyk(&src, w, h, 32, 24);
        let (_, mean) = max_diff(&d2.pixels, &direct);
        assert!(mean < 3.0, "mean {mean:.2}");
    }

    #[test]
    fn rejects_other_component_counts_and_garbage() {
        let rgb = vec![100u8; 16 * 16 * 3];
        let j = Encoder::new(80).encode_rgb(&rgb, 16, 16);
        assert!(decode_cmyk(&j).is_none());
        assert!(decode_cmyk(b"").is_none());
        let src = test_cmyk(24, 20);
        let j = Encoder::new(75).progressive(true).encode_cmyk(
            &src,
            24,
            20,
            Some(AdobeTransform::Ycck),
        );
        for n in 0..j.len() {
            let _ = decode_cmyk(&j[..n]);
        }
        let mut seed = 777u64;
        for _ in 0..1000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let i = (seed >> 33) as usize % j.len();
            let mut m = j.clone();
            m[i] ^= (seed >> 17) as u8 | 1;
            let _ = decode_cmyk(&m);
        }
    }
}
