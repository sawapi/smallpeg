//! smallpeg — a from-scratch, dependency-free JPEG encoder in pure Rust.
//!
//! Color transform, DCT, quantization and Huffman coding are all hand-rolled
//! from the JPEG standard (ITU-T T.81). No dependencies, no `unsafe`, and
//! `wasm32-unknown-unknown` is a first-class target.
//!
//! # What the encoder does by default
//!
//! - **Progressive, chosen by measurement.** The scan script is optimized
//!   per image (band split points, point-transform depth, DC precision are
//!   each priced with real statistics passes); both progressive and baseline
//!   entropy streams are then rendered and the smaller one ships. Set
//!   [`Encoder::progressive(false)`](Encoder::progressive) to force baseline
//!   sequential output for targets that cannot take progressive files.
//! - **Per-image optimized Huffman tables**, always on — the encoder is
//!   two-pass by design: pass 1 collects symbol statistics, pass 2 emits the
//!   entropy-coded stream, which is also the structure trellis quantization
//!   needs.
//! - **Trellis quantization.** Every AC coefficient is re-decided by dynamic
//!   programming against the real run-length/EOB structure; the DC
//!   coefficients form per-component DPCM chains and get their own Viterbi
//!   pass.
//! - **AAN scaled DCT**, with descaling folded into the quantization
//!   divisors and verified bit-for-bit against the naive orthonormal
//!   transform in tests.
//! - **Quality-adaptive quantization tables**, with two presets —
//!   [`QuantPreset::Photo`] (the default) and [`QuantPreset::AnnexK`].
//! - **Clipped-pixel deringing.** Pixels at full white decode to 255
//!   whatever value past 255 they are encoded as, so in blocks that mix
//!   white with darker content they are re-chosen as a smooth continuation
//!   of the neighboring edge rising past the clip point. Quantization
//!   ripple then lands above 255, where the decoder's clamp removes it,
//!   instead of showing as haze around text and line art. Photographs are
//!   essentially unaffected. Off at qualities where a quantization step
//!   reaches 1, where it could push coefficients out of baseline range.
//!
//! # Decoding
//!
//! [`decode_jpeg`] decodes grayscale and color JPEGs (baseline and
//! progressive, any sampling) to 8-bit gray or RGB, following the color
//! space conventions mainstream decoders use and their default
//! interpolating chroma upsampling; [`decode_cmyk`] does the same for
//! four-component files without color conversion (see below). Both take
//! untrusted input and return `None` rather than panic.
//!
//! # Example
//!
//! ```
//! let (width, height) = (64u32, 48u32);
//! let rgb = vec![128u8; (width * height * 3) as usize];
//!
//! let jpeg = smallpeg::Encoder::new(85)
//!     .sampling(smallpeg::Sampling::S420)
//!     .encode_rgb(&rgb, width, height);
//! assert_eq!(&jpeg[..2], &[0xFF, 0xD8]); // SOI
//!
//! // Downsample first when the target is "small" (usually it is):
//! let half = smallpeg::resize_rgb(&rgb, width, height, width / 2, height / 2);
//! let small = smallpeg::Encoder::new(80).encode_rgb(&half, width / 2, height / 2);
//! assert!(small.len() < jpeg.len());
//!
//! // Shrink an existing JPEG without touching its pixels:
//! if let Some(opt) = smallpeg::optimize_jpeg(&jpeg) {
//!     assert!(opt.len() <= jpeg.len());
//! }
//! ```
//!
//! # CMYK
//!
//! Four-component JPEGs (CMYK and YCCK, common in print-oriented PDFs) can
//! be decoded, resized and re-encoded without ever being converted to RGB:
//! [`decode_cmyk`] returns the stored channel values and the file's Adobe
//! APP14 coding, [`resize_cmyk`] filters each channel linearly, and
//! [`Encoder::encode_cmyk`] writes them back with an APP14 that matches
//! the coding it performed. Whether the values are inverted ("Adobe" CMYK)
//! is never interpreted, so it survives the round trip as long as the
//! decoded [`CmykImage::adobe`] is passed back.
//!
//! ```
//! # let src = smallpeg::Encoder::new(90).encode_cmyk(
//! #     &vec![128u8; 32 * 32 * 4], 32, 32, Some(smallpeg::AdobeTransform::Ycck));
//! let img = smallpeg::decode_cmyk(&src).expect("a four-component JPEG");
//! let (w, h) = (img.width / 2, img.height / 2);
//! let small = smallpeg::resize_cmyk(&img.pixels, img.width, img.height, w, h);
//! let jpeg = smallpeg::Encoder::new(85).encode_cmyk(&small, w, h, img.adobe);
//! # assert_eq!(smallpeg::decode_cmyk(&jpeg).unwrap().adobe, img.adobe);
//! ```
//!
//! # Color profiles
//!
//! The encoder takes raw RGB bytes and never sees the source file, so it
//! cannot know what color space those bytes are in. Output carries no ICC
//! profile unless you attach one with [`Encoder::icc_profile`], and
//! decoders then read it as sRGB. Pixels in another space (Display P3 is
//! the usual case: iPhone photos, macOS screenshots and renders) come out
//! with visibly shifted colors unless the source's profile is forwarded or
//! the pixels are converted to sRGB first. Most image decoding crates hand
//! back raw pixels and leave the profile aside, so it is easy to drop by
//! accident. [`optimize_jpeg`] is unaffected: it keeps the input's APPn
//! segments as they are.
//!
//! # Panics
//!
//! The encoding and resizing entry points take the pixel buffer and its
//! dimensions as separate arguments, and panic when they disagree — see the
//! `# Panics` section on each. [`optimize_jpeg`], [`upright_jpeg`] and the
//! decoders take untrusted bytes instead and never panic: they return `None` for anything
//! they cannot handle safely. Memory follows the declared frame size,
//! not the file size, so for untrusted input check [`jpeg_info`] or pass
//! [`Limits`] with a pixel budget to the `_with` variants.

#![forbid(unsafe_code)]
// The DSP-style loops in this crate index several arrays with one shared
// position; the iterator forms clippy suggests obscure the math.
#![allow(clippy::needless_range_loop)]

mod cmyk;
mod component;
mod dct;
mod decode;
mod dering;
mod document;
mod huffman;
mod meta;
mod optimize;
mod parallel;
mod progressive;
mod quant;
mod resize;
mod symbols;
mod trellis;
mod upright;

pub use cmyk::{AdobeTransform, CmykImage, decode_cmyk, decode_cmyk_with};
pub use decode::{DecodedImage, decode_jpeg, decode_jpeg_with};
pub use meta::{apply_orientation, strip_metadata};
pub use optimize::{
    JpegInfo, Limits, jpeg_info, optimize_jpeg, optimize_jpeg_baseline,
    optimize_jpeg_baseline_with, optimize_jpeg_with,
};
pub use resize::{
    CropRect, crop, fit_within, resize_cmyk, resize_gray, resize_gray_linear, resize_rgb,
    resize_rgb_linear,
};
pub use upright::{upright_jpeg, upright_jpeg_with};

use crate::component::{Component, dct_quant, interleave_order};
use crate::dct::dequant_scale;
use crate::document::encode_document;
use crate::quant::{BASE_QT_CHROMA, BASE_QT_LUMA, BASE_QT_PHOTO, scaled_quant_table};

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
compile_error!("feature \"parallel\" is not supported on wasm32 targets (no std threads)");

/// Chroma subsampling mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampling {
    /// Full chroma resolution (4:4:4).
    S444,
    /// 2x2 chroma subsampling (4:2:0).
    S420,
}

/// Base quantization table preset (scaled by the `quality` setting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantPreset {
    /// The ITU T.81 Annex K example tables. The better pick for
    /// documents, scans, and noisy content at high quality.
    AnnexK,
    /// One fine-grained table shared by luma and chroma (widely used
    /// numeric data; provenance in THIRD_PARTY.md). Noticeably better on
    /// photographs — smooth gradients and low quality especially.
    Photo,
}

/// JPEG encoder, configured by chaining the builder methods below.
///
/// Built with [`Encoder::new`] from a quality value; every other setting has
/// a measured default, so `Encoder::new(q).encode_rgb(..)` is the intended
/// way to use this crate.
pub struct Encoder {
    quality: u8,
    sampling: Sampling,
    quant: QuantPreset,
    /// None = default lambda with the per-image gradient gate enabled;
    /// Some = fixed lambda, gate disabled (predictable for tuning).
    lambda: Option<f32>,
    deadzone: (f32, f32),
    /// AQ-lite (ringing control): (flat pixel-variance threshold, strong
    /// threshold, lambda gain < 1 applied to at-risk blocks, lambda boost
    /// >= 1 applied everywhere else). gain >= 1 disables.
    aq_lite: (f32, f32, f32, f32),
    /// Experimental table-shape tweaks: (chroma step scale, luma HF blend
    /// toward flat). (1.0, 0.0) = untouched presets.
    table_tweak: (f32, f32),
    /// None = pick automatically from the quant preset.
    chroma_lambda_scale: Option<f32>,
    /// Clipped-pixel deringing on luma: (overshoot cap in sample units,
    /// solver sweep limit, also treat black). None = off.
    dering: Option<(f32, u32, bool)>,
    /// CMYK deringing component mask override (None = per-coding default).
    cmyk_dering: Option<u8>,
    progressive: bool,
    /// Worker threads for the parallel stages; 0 = automatic.
    threads: usize,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

/// Default rate-distortion trade-off for trellis quantization, in units of
/// (quantization steps)^2 per bit. Tuned on the bench corpus: smaller keeps
/// more coefficients (higher PSNR), larger drops more (smaller files).
const DEFAULT_LAMBDA: f32 = 0.06;

/// AQ-lite (ringing control), kept off: reduced lambda at strong-edge /
/// flat-neighbor sites, optional boost elsewhere. Measured against the
/// SSIM2 + butteraugli-3-norm gate and rejected — the one-sided form is
/// bounded by nearest rounding (baseline JPEG has one table), and the
/// two-sided form pays more in global SSIM2 than it wins in worst-case
/// butteraugli, which as a max statistic does not respond consistently
/// to a binary block classifier. Left as a documented negative result.
const AQ_FLAT_VAR: f32 = 1.0;
const AQ_STRONG_VAR: f32 = 1000.0;
const AQ_GAIN: f32 = 1.0; // >= 1.0 = disabled
const AQ_BOOST: f32 = 1.0;

/// Adaptive deadzone (visual masking): busy blocks hide quantization
/// noise, so their trellis lambda is scaled up by (activity)^alpha, capped.
/// Flat blocks stay at the base lambda — nearest rounding is already the
/// most precise thing the syntax allows there, so masking is one-sided.
const DEADZONE_ALPHA: f32 = 0.0; // masking-direction boost lost to SSIM2; keep off
const DEADZONE_CAP: f32 = 2.0;

/// Quality-dependent luma high-frequency flattening (Photo preset
/// only): at high quality the preset's steep HF steps are too coarse
/// relative to its fine low band, and geometrically blending the table
/// toward flat wins on textured and document content. Gradient-dominant
/// images are exempt — measured per image from the quant-independent DCT
/// energy split. Ramp: 0 at q<=60 up to HF_BLEND_MAX at q>=90.
const HF_BLEND_MAX: f32 = 0.25;

/// Gradient-vs-texture classifier for the HF-blend exemption: gradients
/// concentrate pre-quant DCT energy in the first zigzag positions AND
/// quantize to very few nonzero ACs; texture fails at least one of the two.
/// Whether `is_gradient` will be consulted for this encoder's settings:
/// the quality-adaptive HF blend is active (Photo preset, untweaked tables,
/// quality above 60, i.e. a nonzero blend). Only then does the judged
/// component need its energy split accumulated.
fn tracks_gradient(e: &Encoder) -> bool {
    e.table_tweak == (1.0, 0.0) && e.quant == QuantPreset::Photo && e.quality > 60
}

/// The share of AC energy in the lowest frequencies from which a component
/// counts as a gradient.
const GRADIENT_RATIO: f64 = 0.70;

fn gradient_ratio(e_low: f64, e_all: f64) -> f64 {
    e_low / e_all.max(f64::MIN_POSITIVE)
}

fn is_gradient(c: &Component) -> bool {
    let ratio = gradient_ratio(c.e_low, c.e_all);
    let nz: usize = c
        .blocks
        .iter()
        .map(|b| b[1..].iter().filter(|&&v| v != 0).count())
        .sum();
    ratio >= GRADIENT_RATIO && nz as f64 / c.blocks.len() as f64 <= 6.0
}

/// Geometric blend of every step toward 16 — i.e. dynamic-range
/// compression of the table, not a pure HF flatten: steps above 16 get
/// finer, steps below 16 get coarser (reviewer's observation; measured a
/// net win with the LF sacrifice included).
fn compress_luma_table(qt: &[u16; 64], beta: f32) -> [u16; 64] {
    let mut out = *qt;
    for v in out.iter_mut() {
        let blended = ((*v as f32).ln() * (1.0 - beta) + (16f32).ln() * beta).exp();
        *v = (blended.round() as i32).clamp(1, 255) as u16;
    }
    out
}

/// Chroma trellis lambda scale per preset. Annex K's chroma steps are ~6x
/// coarser than luma, so step-unit distortion undervalues the real pixel
/// error of dropping a chroma coefficient — full-strength chroma trellis
/// cost ~2 dB for ~2% there, hence the tiny scale. The Photo preset
/// shares one fine-grained table across components, so its chroma trellis
/// is safe at half strength — and at full strength once the distortion is
/// frequency-weighted (`TRELLIS_FREQ_GAMMA_PHOTO`), which is how the RGB
/// path runs it.
const CHROMA_SCALE_ANNEXK: f32 = 0.05;
const CHROMA_SCALE_PHOTO: f32 = 0.5;
const CHROMA_SCALE_PHOTO_WEIGHTED: f32 = 1.0;

/// Frequency weighting of the AC trellis distortion for the Photo preset
/// on RGB input: coefficient k's squared step-unit error is multiplied by
/// (q_k / geometric mean of the AC steps)^gamma. Plain step units (gamma
/// 0) treat a one-step error the same everywhere; true pixel error (gamma
/// 2) wrecks gradients (see `trellis_ac`). A mild 0.5 makes coarse-step
/// high frequencies a little dearer to drop and fine-step low frequencies
/// a little cheaper, and with it the chroma trellis is best at the luma
/// lambda. Chosen on CLIC 2020 professional at 4:2:0 (0.35-1.0 plateau or
/// worse); on images held out from that choice (CLIC mobile, Kodak,
/// rendered documents) output is 0-2% smaller at 4:2:0 and 2-3.5% at
/// 4:4:4 at equal SSIMULACRA2 / butteraugli, at the same encode time.
/// Annex K, grayscale and CMYK keep plain step units (not measured).
const TRELLIS_FREQ_GAMMA_PHOTO: f32 = 0.5;

/// Chroma quant-step multiplier for the Photo preset at 4:4:4. Photo shares
/// one fine table between luma and chroma, which is balanced at 4:2:0; at
/// 4:4:4 chroma has four times the samples and that table overspends on
/// it. Doubling the chroma steps was 5-10% smaller at equal SSIMULACRA2
/// and 9-14% at equal butteraugli on photos held out from the choice of
/// factor (CLIC 2020 mobile, Kodak), and neutral to slightly better on
/// rendered documents. 2.5 and 3 won further at high quality but lost
/// around SSIMULACRA2 70, documents included. Measured up to quality 95;
/// above that it fades to 1 so quality 100 keeps all-1 tables.
fn photo_444_chroma_factor(quality: u8) -> f32 {
    (1.0 + (100.0 - quality as f32) / 5.0).clamp(1.0, 2.0)
}

/// Clipped-pixel deringing (see `dering`): luma pixels at full white may
/// be pushed up to this far past 255 along the neighboring edge's slope,
/// with at most this many solver sweeps. Measured on rendered documents
/// (Latin and Japanese text, tables, charts): 17-19% smaller at equal
/// SSIMULACRA2 and 10-13% at equal butteraugli, with 48 and 64 tied and
/// 32 slightly behind; photographs are neutral (within 0.2%). Black is
/// left alone: dark ink carried almost none of the gain.
const DERING_CAP: f32 = 48.0;
/// Hard limit on the overshoot, whatever `Encoder::dering` asks for: keeps
/// the coefficient-range argument in `make_dering` valid.
const DERING_CAP_MAX: f32 = 64.0;
/// Largest coefficient magnitude baseline JPEG can code (see `push_block`).
const COEF_MAX: f32 = 1023.0;
const DERING_SWEEPS: u32 = 10;

#[cfg(test)]
thread_local! {
    /// Makes the RGB encoder take its one-band second pass regardless of
    /// the energy it found, so tests can reach that path.
    static FORCE_ONE_BAND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The gradient energy (low, all) the RGB encoder last settled on.
    static LAST_ENERGY: std::cell::Cell<(f64, f64)> = const { std::cell::Cell::new((0.0, 0.0)) };
}

fn force_one_band() -> bool {
    #[cfg(test)]
    return FORCE_ONE_BAND.with(|f| f.get());
    #[cfg(not(test))]
    false
}

/// Output slots for one band of MCU rows: (quantized, pre-rounding) blocks
/// per component, and the gradient energy of each row.
struct RowsOut<'a> {
    y: (&'a mut [[i16; 64]], &'a mut [[f32; 64]]),
    cb: (&'a mut [[i16; 64]], &'a mut [[f32; 64]]),
    cr: (&'a mut [[i16; 64]], &'a mut [[f32; 64]]),
    energy: &'a mut [(f64, f64)],
}

impl Encoder {
    /// New encoder at the given quality, on the usual 1-100 JPEG scale
    /// (values outside it are clamped, not rejected).
    ///
    /// Defaults: 4:2:0 subsampling, [`QuantPreset::Photo`] tables, trellis
    /// quantization on, clipped-pixel deringing on, progressive output.
    pub fn new(quality: u8) -> Self {
        Self {
            quality: quality.clamp(1, 100),
            sampling: Sampling::S420,
            quant: QuantPreset::Photo,
            lambda: None,
            deadzone: (DEADZONE_ALPHA, DEADZONE_CAP),
            aq_lite: (AQ_FLAT_VAR, AQ_STRONG_VAR, AQ_GAIN, AQ_BOOST),
            table_tweak: (1.0, 0.0),
            chroma_lambda_scale: None,
            dering: Some((DERING_CAP, DERING_SWEEPS, false)),
            cmyk_dering: None,
            progressive: true,
            threads: 0,
            icc: None,
            exif: None,
        }
    }

    /// Chroma subsampling mode. Defaults to [`Sampling::S420`].
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = sampling;
        self
    }

    /// Choose the base quantization tables.
    pub fn quant_preset(mut self, preset: QuantPreset) -> Self {
        self.quant = preset;
        self
    }

    fn base_tables(&self) -> (&'static [u16; 64], &'static [u16; 64]) {
        match self.quant {
            QuantPreset::AnnexK => (&BASE_QT_LUMA, &BASE_QT_CHROMA),
            QuantPreset::Photo => (&BASE_QT_PHOTO, &BASE_QT_PHOTO),
        }
    }

    /// Progressive output is the default: the scan script is optimized per
    /// image and the result still has to beat the baseline render to ship.
    /// Pass `false` to force baseline sequential output (single scan,
    /// maximum decoder compatibility — e.g. JPEGs embedded in PDFs, where
    /// progressive support across viewers is not a safe bet).
    ///
    /// This method and the `false` (baseline) mode are stable API:
    /// downstream users pin their compatibility guarantees on it.
    pub fn progressive(mut self, on: bool) -> Self {
        self.progressive = on;
        self
    }

    /// Number of worker threads used with the `parallel` feature. `0` (the
    /// default) uses the available cores, up to 8; a count above 64 is
    /// taken as 64. Small images use fewer than asked for, down to the
    /// calling thread alone, since starting threads would cost more than
    /// it saves. The output is the same for any count; without the feature
    /// everything runs on the calling thread.
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = n;
        self
    }

    /// Embed an ICC color profile (the raw profile bytes) in the output,
    /// as the standard `ICC_PROFILE` APP2 marker sequence — chunked when
    /// the profile exceeds one segment's capacity (65,519 bytes). Profiles
    /// larger than the format's hard limit of 255 chunks (~16.7 MB) are
    /// not emitted. Stable API.
    ///
    /// Without a profile, decoders treat the output as sRGB. When the
    /// pixels come from a non-sRGB source (Display P3 and the like),
    /// forward that source's profile here, or the colors shift. See
    /// [Color profiles](crate#color-profiles).
    ///
    /// ```
    /// # let (rgb, width, height) = (vec![0u8; 16 * 16 * 3], 16u32, 16u32);
    /// // e.g. the profile your image decoder read from the source file
    /// let source_icc: Option<Vec<u8>> = None;
    ///
    /// let mut enc = smallpeg::Encoder::new(85);
    /// if let Some(icc) = &source_icc {
    ///     enc = enc.icc_profile(icc);
    /// }
    /// let jpeg = enc.encode_rgb(&rgb, width, height);
    /// # assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    /// ```
    pub fn icc_profile(mut self, profile: &[u8]) -> Self {
        self.icc = Some(profile.to_vec());
        self
    }

    /// Embed EXIF metadata (the TIFF stream, without the `Exif\0\0`
    /// header) as an APP1 marker. EXIF is defined as a single segment, so
    /// payloads over 65,527 bytes are not emitted. Stable API.
    pub fn exif(mut self, tiff: &[u8]) -> Self {
        self.exif = Some(tiff.to_vec());
        self
    }

    /// Render the APP1 (EXIF) and APP2 (ICC) segments placed right after
    /// the JFIF APP0 header, in that order — the layout common tools
    /// produce when a file carries both.
    fn app_markers(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(tiff) = self.exif.as_deref()
            && tiff.len() <= 65533 - 6
        {
            out.extend_from_slice(&[0xFF, 0xE1]);
            out.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
            out.extend_from_slice(b"Exif\0\0");
            out.extend_from_slice(tiff);
        }
        if let Some(icc) = self.icc.as_deref() {
            const CHUNK: usize = 65533 - 14; // len field + "ICC_PROFILE\0" + seq/count
            let chunks = icc.chunks(CHUNK.max(1));
            let n = chunks.len();
            if (1..=255).contains(&n) && !icc.is_empty() {
                for (i, c) in chunks.enumerate() {
                    out.extend_from_slice(&[0xFF, 0xE2]);
                    out.extend_from_slice(&((2 + 12 + 2 + c.len()) as u16).to_be_bytes());
                    out.extend_from_slice(b"ICC_PROFILE\0");
                    out.push(i as u8 + 1);
                    out.push(n as u8);
                    out.extend_from_slice(c);
                }
            }
        }
        out
    }

    /// Override the trellis rate-distortion lambda. Exposed for tuning;
    /// the default is right for almost everyone. An explicit lambda also
    /// disables the per-image gradient escalation, so measurements see
    /// exactly the value they set.
    #[doc(hidden)]
    pub fn trellis_lambda(mut self, lambda: f32) -> Self {
        self.lambda = Some(lambda.max(0.0));
        self
    }

    /// Override the chroma lambda scale. Exposed for tuning.
    #[doc(hidden)]
    pub fn trellis_chroma_scale(mut self, scale: f32) -> Self {
        self.chroma_lambda_scale = Some(scale.max(0.0));
        self
    }

    /// Override the AQ-lite ringing-control parameters. Exposed for tuning.
    #[doc(hidden)]
    pub fn aq_lite(mut self, flat_var: f32, strong_var: f32, gain: f32, boost: f32) -> Self {
        self.aq_lite = (
            flat_var.max(0.0),
            strong_var.max(0.0),
            gain.max(0.0),
            boost.max(1.0),
        );
        self
    }

    /// Override the adaptive-deadzone masking strength. Exposed for tuning.
    #[doc(hidden)]
    pub fn deadzone(mut self, alpha: f32, cap: f32) -> Self {
        self.deadzone = (alpha.max(0.0), cap.max(1.0));
        self
    }

    /// Experimental quant-table shape tweaks. Exposed for tuning.
    #[doc(hidden)]
    pub fn table_tweak(mut self, chroma_scale: f32, luma_hf_blend: f32) -> Self {
        self.table_tweak = (chroma_scale.max(0.05), luma_hf_blend.clamp(0.0, 1.0));
        self
    }

    /// Turn clipped-pixel deringing on (the default) or off.
    ///
    /// It pays off on content with exact-white areas next to hard edges:
    /// rendered documents, screenshots, line art (17–19% smaller at equal
    /// SSIMULACRA2 on rendered documents). Images that are themselves
    /// decoded JPEGs, such as those re-compressed out of PDFs, measured
    /// neutral while encoding about 25% slower, so turning it off there
    /// saves time at no cost. Stable API.
    pub fn deringing(mut self, on: bool) -> Self {
        self.dering = on.then_some((DERING_CAP, DERING_SWEEPS, false));
        self
    }

    /// Clipped-pixel deringing parameters. Exposed for tuning; pass
    /// `sweeps == 0` to turn it off.
    #[doc(hidden)]
    pub fn dering(mut self, cap: f32, sweeps: u32, low: bool) -> Self {
        self.dering = (sweeps > 0).then_some((cap, sweeps, low));
        self
    }

    /// Which components of `encode_cmyk` output are deringed (bit i =
    /// component i). Exposed for tuning.
    #[doc(hidden)]
    pub fn cmyk_dering(mut self, mask: u8) -> Self {
        self.cmyk_dering = Some(mask);
        self
    }

    /// Deringing for a component quantized with `qt`, if enabled. Off when
    /// any step is 1: samples pushed past the clip points widen a block's
    /// range beyond 255, and at step 1 its coefficients can then exceed
    /// what baseline JPEG can code (AC size 10, |c| <= 1023). With every
    /// step at 2 or more, even the widest overshot block (range 255 plus
    /// twice the cap) stays well inside. The ringing it removes is
    /// negligible at those qualities anyway.
    fn make_dering(&self, qt: &[u16; 64]) -> Option<dering::Dering> {
        if qt.iter().any(|&q| q < 2) {
            return None;
        }
        self.dering
            .map(|(cap, sweeps, low)| dering::Dering::new(cap.min(DERING_CAP_MAX), sweeps, low))
    }

    fn chroma_scale(&self) -> f32 {
        self.chroma_lambda_scale.unwrap_or(match self.quant {
            QuantPreset::AnnexK => CHROMA_SCALE_ANNEXK,
            QuantPreset::Photo => CHROMA_SCALE_PHOTO,
        })
    }

    /// (chroma lambda scale, distortion frequency-weight gamma) for the RGB
    /// path, where the Photo preset pairs weighting with a full-strength
    /// chroma trellis. An explicit `chroma_lambda_scale` still wins.
    fn rgb_trellis_tuning(&self) -> (f32, f32) {
        match self.quant {
            QuantPreset::AnnexK => (self.chroma_scale(), 0.0),
            QuantPreset::Photo => (
                self.chroma_lambda_scale
                    .unwrap_or(CHROMA_SCALE_PHOTO_WEIGHTED),
                TRELLIS_FREQ_GAMMA_PHOTO,
            ),
        }
    }

    /// Encode interleaved 8-bit RGB pixels.
    ///
    /// # Panics
    ///
    /// Panics if `rgb.len()` is not exactly `width * height * 3`, or if
    /// either dimension is 0 or above 65535 (the JPEG frame-header limit).
    pub fn encode_rgb(&self, rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
        assert_eq!(rgb.len(), width as usize * height as usize * 3);
        assert!(width > 0 && height > 0 && width <= 65535 && height <= 65535);
        let (w, h) = (width as usize, height as usize);
        let (base_l, base_c) = self.base_tables();
        let mut qt_luma = scaled_quant_table(base_l, self.quality);
        let mut qt_chroma = scaled_quant_table(base_c, self.quality);
        let (tw_c, tw_b) = self.table_tweak;
        // The tweak overrides the automatic 4:4:4 chroma scale, so tuning
        // runs see exactly the factor they set.
        let chroma_factor = if tw_c != 1.0 {
            tw_c
        } else if self.sampling == Sampling::S444 && self.quant == QuantPreset::Photo {
            photo_444_chroma_factor(self.quality)
        } else {
            1.0
        };
        if chroma_factor != 1.0 {
            for v in qt_chroma.iter_mut() {
                *v = ((*v as f32 * chroma_factor).round() as i32).clamp(1, 255) as u16;
            }
        }
        if tw_b > 0.0 {
            for v in qt_luma.iter_mut() {
                let blended = ((*v as f32).ln() * (1.0 - tw_b) + (16f32).ln() * tw_b).exp();
                *v = (blended.round() as i32).clamp(1, 255) as u16;
            }
        }
        // Luma block grid must cover whole MCUs (2x2 blocks in 4:2:0).
        let (h2, v2) = match self.sampling {
            Sampling::S444 => (1usize, 1usize),
            Sampling::S420 => (2, 2),
        };
        let mcu_w = ceil_div(w, 8 * h2);
        let mcu_h = ceil_div(h, 8 * v2);
        let lbw = mcu_w * h2;
        let (cw, chh) = (ceil_div(w, h2), ceil_div(h, v2)); // chroma sample dims

        let auto_hf = self.table_tweak == (1.0, 0.0) && self.quant == QuantPreset::Photo;
        let beta_q = HF_BLEND_MAX * ((self.quality as f32 - 60.0) / 30.0).clamp(0.0, 1.0);

        let build = |qt_l: &[u16; 64], qt_c: &[u16; 64]| -> Vec<Component> {
            let recip_l = dequant_scale(qt_l);
            let recip_c = dequant_scale(qt_c);
            let der = self.make_dering(qt_l);

            let mut comps = vec![
                Component::with_grid(
                    lbw,
                    ceil_div(w, 8),
                    ceil_div(h, 8),
                    h2 as u8,
                    v2 as u8,
                    0,
                    lbw * mcu_h * v2,
                ),
                Component::with_grid(
                    mcu_w,
                    ceil_div(cw, 8),
                    ceil_div(chh, 8),
                    1,
                    1,
                    1,
                    mcu_w * mcu_h,
                ),
                Component::with_grid(
                    mcu_w,
                    ceil_div(cw, 8),
                    ceil_div(chh, 8),
                    1,
                    1,
                    1,
                    mcu_w * mcu_h,
                ),
            ];
            comps[0].energy = tracks_gradient(self);

            // Color-convert and DCT one MCU row at a time: the strips below are
            // the only pixel-domain buffers, so peak memory stays proportional
            // to the coefficients, not to three full f32 planes. Strips are
            // padded to whole MCUs (edge replication baked in) so block
            // extraction is branch-free.
            //
            // MCU rows are independent, so bands of rows are encoded in
            // parallel, each writing its blocks in place.
            //
            // The gradient energy is one running sum over all blocks in
            // order, and the sum's rounding depends on that order. A single
            // band keeps it (`carry`: each row goes on from the one before).
            // Several bands sum each row by itself, then the rows in order,
            // which does not depend on how the rows were split but can
            // round differently: enough to matter only when the result is
            // at the threshold, and then the rows are done again as one band.
            let ypw = lbw * 8;
            let cpw = mcu_w * 8;
            let (luma_row, chroma_row) = (lbw * v2, mcu_w);
            let track_energy = comps[0].energy;
            for (c, row) in comps.iter_mut().zip([luma_row, chroma_row, chroma_row]) {
                c.blocks = vec![[0; 64]; row * mcu_h];
                c.raw = vec![[0.0; 64]; row * mcu_h];
            }
            let mut row_energy = vec![(0f64, 0f64); mcu_h];
            let encode_rows = |first: usize, out: RowsOut<'_>, carry: bool| {
                let RowsOut { y, cb, cr, energy } = out;
                let mut ystrip = vec![0f32; ypw * 8 * v2];
                let mut cbstrip = vec![0f32; cpw * 8];
                let mut crstrip = vec![0f32; cpw * 8];
                let mut px = [0f32; 64];
                for k in 0..energy.len() {
                    if carry && k > 0 {
                        energy[k] = energy[k - 1];
                    }
                    let e = &mut energy[k];
                    let my = first + k;
                    for r in 0..8 * v2 {
                        let sy = (my * 8 * v2 + r).min(h - 1);
                        let src = &rgb[sy * w * 3..][..w * 3];
                        let dst = &mut ystrip[r * ypw..][..ypw];
                        for x in 0..w {
                            let (rr, gg, bb) = (
                                src[x * 3] as f32,
                                src[x * 3 + 1] as f32,
                                src[x * 3 + 2] as f32,
                            );
                            dst[x] = 0.299 * rr + 0.587 * gg + 0.114 * bb;
                        }
                        let last = dst[w - 1];
                        dst[w..].fill(last);
                    }
                    let cbcr = |p: &[u8]| -> (f32, f32) {
                        let (rr, gg, bb) = (p[0] as f32, p[1] as f32, p[2] as f32);
                        (
                            -0.168736 * rr - 0.331264 * gg + 0.5 * bb + 128.0,
                            0.5 * rr - 0.418688 * gg - 0.081312 * bb + 128.0,
                        )
                    };
                    if h2 == 2 {
                        // 4:2:0 — per-pixel conversion, then the same box average
                        // (identical arithmetic order to the old plane pipeline).
                        for r in 0..8 {
                            // Clamp in chroma sample space so padded rows replicate
                            // the last real chroma row (not a recomputed variant).
                            let cy = (my * 8 + r).min(chh - 1);
                            let y0 = (cy * 2).min(h - 1);
                            let y1 = (cy * 2 + 1).min(h - 1);
                            for cx in 0..cw {
                                let x0 = (cx * 2).min(w - 1);
                                let x1 = (cx * 2 + 1).min(w - 1);
                                let (b00, r00) = cbcr(&rgb[(y0 * w + x0) * 3..][..3]);
                                let (b01, r01) = cbcr(&rgb[(y0 * w + x1) * 3..][..3]);
                                let (b10, r10) = cbcr(&rgb[(y1 * w + x0) * 3..][..3]);
                                let (b11, r11) = cbcr(&rgb[(y1 * w + x1) * 3..][..3]);
                                cbstrip[r * cpw + cx] = (b00 + b01 + b10 + b11) * 0.25;
                                crstrip[r * cpw + cx] = (r00 + r01 + r10 + r11) * 0.25;
                            }
                            let (lb, lr) = (cbstrip[r * cpw + cw - 1], crstrip[r * cpw + cw - 1]);
                            cbstrip[r * cpw + cw..(r + 1) * cpw].fill(lb);
                            crstrip[r * cpw + cw..(r + 1) * cpw].fill(lr);
                        }
                    } else {
                        for r in 0..8 {
                            let sy = (my * 8 + r).min(h - 1);
                            for x in 0..w {
                                let (b, rr) = cbcr(&rgb[(sy * w + x) * 3..][..3]);
                                cbstrip[r * cpw + x] = b;
                                crstrip[r * cpw + x] = rr;
                            }
                            let (lb, lr) = (cbstrip[r * cpw + w - 1], crstrip[r * cpw + w - 1]);
                            cbstrip[r * cpw + w..(r + 1) * cpw].fill(lb);
                            crstrip[r * cpw + w..(r + 1) * cpw].fill(lr);
                        }
                    }
                    let (yb, yr) = (
                        &mut y.0[k * luma_row..][..luma_row],
                        &mut y.1[k * luma_row..][..luma_row],
                    );
                    let mut slot = 0;
                    for br in 0..v2 {
                        for bx in 0..lbw {
                            for yy in 0..8 {
                                let row = &ystrip[(br * 8 + yy) * ypw + bx * 8..][..8];
                                for (d, &sv) in px[yy * 8..yy * 8 + 8].iter_mut().zip(row) {
                                    *d = sv - 128.0;
                                }
                            }
                            if let Some(d) = &der {
                                d.apply(&mut px);
                            }
                            dct_quant(
                                &mut px,
                                &recip_l,
                                &mut yb[slot],
                                &mut yr[slot],
                                track_energy.then_some(&mut *e),
                            );
                            slot += 1;
                        }
                    }
                    for (strip, (blocks, raw)) in [
                        (
                            &cbstrip,
                            (
                                &mut cb.0[k * chroma_row..][..chroma_row],
                                &mut cb.1[k * chroma_row..][..chroma_row],
                            ),
                        ),
                        (
                            &crstrip,
                            (
                                &mut cr.0[k * chroma_row..][..chroma_row],
                                &mut cr.1[k * chroma_row..][..chroma_row],
                            ),
                        ),
                    ] {
                        for bx in 0..mcu_w {
                            for yy in 0..8 {
                                let row = &strip[yy * cpw + bx * 8..][..8];
                                for (d, &sv) in px[yy * 8..yy * 8 + 8].iter_mut().zip(row) {
                                    *d = sv - 128.0;
                                }
                            }
                            dct_quant(&mut px, &recip_c, &mut blocks[bx], &mut raw[bx], None);
                        }
                    }
                }
            };
            let mut run = |workers: usize, row_energy: &mut [(f64, f64)]| {
                let [cy, ccb, ccr] = comps.as_mut_slice() else {
                    unreachable!("RGB encodes three components")
                };
                let band = mcu_h.div_ceil(workers);
                let bands = cy
                    .blocks
                    .chunks_mut(band * luma_row)
                    .zip(cy.raw.chunks_mut(band * luma_row))
                    .zip(
                        ccb.blocks
                            .chunks_mut(band * chroma_row)
                            .zip(ccb.raw.chunks_mut(band * chroma_row)),
                    )
                    .zip(
                        ccr.blocks
                            .chunks_mut(band * chroma_row)
                            .zip(ccr.raw.chunks_mut(band * chroma_row)),
                    )
                    .zip(row_energy.chunks_mut(band))
                    .enumerate()
                    .map(|(i, ((((yb, yr), cb), cr), energy))| {
                        (
                            i * band,
                            RowsOut {
                                y: (yb, yr),
                                cb,
                                cr,
                                energy,
                            },
                        )
                    });
                parallel::for_each(bands, |(first, out)| encode_rows(first, out, band == mcu_h));
            };
            let blocks = (luma_row + 2 * chroma_row) * mcu_h;
            let workers = parallel::threads(self.threads, blocks).min(mcu_h);
            run(workers, &mut row_energy);
            if track_energy {
                // The margin is far above what the order of a sum of this
                // many terms can change (about 1e-6 at the largest frame).
                const MARGIN: f64 = 1e-4;
                let mut total = row_energy[mcu_h - 1];
                if workers > 1 {
                    total = (0f64, 0f64);
                    for (l, a) in &row_energy {
                        total.0 += l;
                        total.1 += a;
                    }
                    let ratio = gradient_ratio(total.0, total.1);
                    if (ratio - GRADIENT_RATIO).abs() < MARGIN || force_one_band() {
                        row_energy.fill((0.0, 0.0));
                        run(1, &mut row_energy);
                        total = row_energy[mcu_h - 1];
                    }
                }
                (comps[0].e_low, comps[0].e_all) = total;
                #[cfg(test)]
                LAST_ENERGY.with(|e| e.set(total));
            }
            comps
        };

        let mut qt_used = if auto_hf && beta_q > 0.0 {
            compress_luma_table(&qt_luma, beta_q)
        } else {
            qt_luma
        };
        let mut comps = build(&qt_used, &qt_chroma);
        if auto_hf && beta_q > 0.0 && is_gradient(&comps[0]) {
            // Gradient-dominant image: the flattened HF only wastes
            // bits here. Rebuild with the untouched preset shape.
            qt_used = qt_luma;
            comps = build(&qt_used, &qt_chroma);
        }
        let order = interleave_order(&comps, mcu_w, mcu_h);
        let (chroma_scale, freq_gamma) = self.rgb_trellis_tuning();
        encode_document(
            comps,
            &order,
            &[qt_used, qt_chroma],
            width as u16,
            height as u16,
            &self.app_markers(),
            self.lambda.unwrap_or(DEFAULT_LAMBDA),
            chroma_scale,
            freq_gamma,
            self.progressive,
            self.lambda.is_none(),
            self.deadzone,
            self.aq_lite,
            true,
            self.threads,
        )
    }

    /// Encode 8-bit grayscale pixels.
    ///
    /// # Panics
    ///
    /// Panics if `gray.len()` is not exactly `width * height`, or if either
    /// dimension is 0 or above 65535 (the JPEG frame-header limit).
    pub fn encode_gray(&self, gray: &[u8], width: u32, height: u32) -> Vec<u8> {
        assert_eq!(gray.len(), width as usize * height as usize);
        assert!(width > 0 && height > 0 && width <= 65535 && height <= 65535);
        let (w, h) = (width as usize, height as usize);
        let qt = scaled_quant_table(self.base_tables().0, self.quality);
        let bw = ceil_div(w, 8);
        let bh = ceil_div(h, 8);
        let build = |qt_l: &[u16; 64]| -> Component {
            let recip = dequant_scale(qt_l);
            let der = self.make_dering(qt_l);
            let mut c = Component::with_grid(bw, bw, bh, 1, 1, 0, bw * bh);
            c.energy = tracks_gradient(self);
            let mut px = [0f32; 64];
            for by in 0..bh {
                for bx in 0..bw {
                    if bx * 8 + 8 <= w && by * 8 + 8 <= h {
                        for yy in 0..8 {
                            let row = &gray[(by * 8 + yy) * w + bx * 8..][..8];
                            for (d, &sv) in px[yy * 8..yy * 8 + 8].iter_mut().zip(row) {
                                *d = sv as f32 - 128.0;
                            }
                        }
                    } else {
                        for yy in 0..8 {
                            for xx in 0..8 {
                                let sx = (bx * 8 + xx).min(w - 1);
                                let sy = (by * 8 + yy).min(h - 1);
                                px[yy * 8 + xx] = gray[sy * w + sx] as f32 - 128.0;
                            }
                        }
                    }
                    if let Some(d) = &der {
                        d.apply(&mut px);
                    }
                    c.push_block(&mut px, &recip);
                }
            }
            c
        };
        let auto_hf = self.table_tweak == (1.0, 0.0) && self.quant == QuantPreset::Photo;
        let beta_q = HF_BLEND_MAX * ((self.quality as f32 - 60.0) / 30.0).clamp(0.0, 1.0);
        let mut qt_used = if auto_hf && beta_q > 0.0 {
            compress_luma_table(&qt, beta_q)
        } else {
            qt
        };
        let mut c = build(&qt_used);
        if auto_hf && beta_q > 0.0 && is_gradient(&c) {
            qt_used = qt;
            c = build(&qt_used);
        }
        let order: Vec<(usize, usize)> = (0..bw * bh).map(|i| (0, i)).collect();
        encode_document(
            vec![c],
            &order,
            &[qt_used],
            width as u16,
            height as u16,
            &self.app_markers(),
            self.lambda.unwrap_or(DEFAULT_LAMBDA),
            self.chroma_scale(),
            0.0,
            self.progressive,
            self.lambda.is_none(),
            self.deadzone,
            self.aq_lite,
            true,
            self.threads,
        )
    }
}

// ---------------------------------------------------------------- helpers

fn ceil_div(a: usize, b: usize) -> usize {
    a.div_ceil(b)
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quant::ZIGZAG;

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = x as f64 - y as f64;
                d * d
            })
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }

    fn test_image(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.push((x * 255 / w) as u8);
                rgb.push((y * 255 / h) as u8);
                rgb.push(((x + y) * 128 / (w + h)) as u8);
            }
        }
        rgb
    }

    #[test]
    fn roundtrip_rgb_444() {
        let (w, h) = (65u32, 41u32); // non-multiple-of-8 on purpose
        let rgb = test_image(w, h);
        let jpg = Encoder::new(90)
            .sampling(Sampling::S444)
            .encode_rgb(&rgb, w, h);
        let img = image::load_from_memory(&jpg).expect("decodable");
        assert_eq!((img.width(), img.height()), (w, h));
        let dec = img.to_rgb8();
        assert!(psnr(&rgb, dec.as_raw()) > 30.0, "psnr too low");
    }

    /// The (id, natural-order table) pairs of every DQT in `jpg`.
    fn dqt_tables(jpg: &[u8]) -> Vec<(u8, [u16; 64])> {
        let mut out = Vec::new();
        let mut i = 2;
        while i + 4 <= jpg.len() && jpg[i] == 0xFF && jpg[i + 1] != 0xDA {
            let len = u16::from_be_bytes([jpg[i + 2], jpg[i + 3]]) as usize;
            if jpg[i + 1] == 0xDB {
                let mut t = [0u16; 64];
                for k in 0..64 {
                    t[ZIGZAG[k]] = jpg[i + 5 + k] as u16;
                }
                out.push((jpg[i + 4], t));
            }
            i += 2 + len;
        }
        out
    }

    #[test]
    fn rgb_trellis_tuning_follows_preset() {
        let photo = Encoder::new(80);
        assert_eq!(
            photo.rgb_trellis_tuning(),
            (CHROMA_SCALE_PHOTO_WEIGHTED, TRELLIS_FREQ_GAMMA_PHOTO)
        );
        let annexk = Encoder::new(80).quant_preset(QuantPreset::AnnexK);
        assert_eq!(annexk.rgb_trellis_tuning(), (CHROMA_SCALE_ANNEXK, 0.0));
        let manual = Encoder::new(80).trellis_chroma_scale(0.25);
        assert_eq!(manual.rgb_trellis_tuning().0, 0.25);
    }

    #[test]
    fn photo_444_doubles_chroma_steps() {
        let (w, h) = (64u32, 48u32);
        let rgb = test_image(w, h);
        let tables =
            |q: u8, s: Sampling| dqt_tables(&Encoder::new(q).sampling(s).encode_rgb(&rgb, w, h));
        for q in [50u8, 75, 95] {
            let (t420, t444) = (tables(q, Sampling::S420), tables(q, Sampling::S444));
            assert_eq!(t420[0], t444[0], "luma unchanged at q{q}");
            for k in 0..64 {
                assert_eq!(t444[1].1[k], (t420[1].1[k] * 2).min(255), "q{q} step {k}");
            }
        }
        // Quality 100 keeps all-1 tables; AnnexK is untouched.
        assert!(
            tables(100, Sampling::S444)
                .iter()
                .all(|(_, t)| t.iter().all(|&v| v == 1))
        );
        let ak = |s: Sampling| {
            dqt_tables(
                &Encoder::new(75)
                    .quant_preset(QuantPreset::AnnexK)
                    .sampling(s)
                    .encode_rgb(&rgb, w, h),
            )
        };
        assert_eq!(ak(Sampling::S420), ak(Sampling::S444));
    }

    #[test]
    fn roundtrip_rgb_420() {
        let (w, h) = (130u32, 97u32);
        let rgb = test_image(w, h);
        let jpg = Encoder::new(85).encode_rgb(&rgb, w, h);
        let img = image::load_from_memory(&jpg).expect("decodable");
        assert_eq!((img.width(), img.height()), (w, h));
        let dec = img.to_rgb8();
        assert!(psnr(&rgb, dec.as_raw()) > 30.0, "psnr too low");
    }

    #[test]
    fn roundtrip_gray() {
        let (w, h) = (100u32, 60u32);
        let gray: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let jpg = Encoder::new(90).encode_gray(&gray, w, h);
        let img = image::load_from_memory(&jpg).expect("decodable");
        assert_eq!((img.width(), img.height()), (w, h));
    }

    #[test]
    fn flat_image_is_tiny() {
        let rgb = vec![128u8; 64 * 64 * 3];
        let jpg = Encoder::new(80).encode_rgb(&rgb, 64, 64);
        assert!(
            jpg.len() < 1500,
            "flat image should compress hard: {}",
            jpg.len()
        );
    }
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    fn segments(j: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pos = 2;
        while j[pos + 1] != 0xDA {
            let m = j[pos + 1];
            let len = ((j[pos + 2] as usize) << 8) | j[pos + 3] as usize;
            out.push((m, j[pos + 4..pos + 2 + len].to_vec()));
            pos += 2 + len;
        }
        out
    }

    fn test_rgb(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let x = (i % w) * 255 / w;
                let y = (i / w) * 255 / h;
                [x as u8, y as u8, 128]
            })
            .collect()
    }

    #[test]
    fn icc_and_exif_are_embedded_and_survive_optimize() {
        let rgb = test_rgb(40, 30);
        let icc: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let exif: Vec<u8> = b"II*\0test-tiff-stream".to_vec();
        let j = Encoder::new(80)
            .icc_profile(&icc)
            .exif(&exif)
            .encode_rgb(&rgb, 40, 30);
        let segs = segments(&j);
        let app1: Vec<_> = segs.iter().filter(|s| s.0 == 0xE1).collect();
        assert_eq!(app1.len(), 1);
        assert_eq!(&app1[0].1[..6], b"Exif\0\0");
        assert_eq!(&app1[0].1[6..], &exif[..]);
        let app2: Vec<_> = segs.iter().filter(|s| s.0 == 0xE2).collect();
        assert_eq!(app2.len(), 1);
        assert_eq!(&app2[0].1[..12], b"ICC_PROFILE\0");
        assert_eq!(&app2[0].1[12..14], &[1, 1]);
        assert_eq!(&app2[0].1[14..], &icc[..]);
        // Pixels are unaffected by metadata.
        let plain = Encoder::new(80).encode_rgb(&rgb, 40, 30);
        let a = image::load_from_memory(&j).unwrap().to_rgb8();
        let b = image::load_from_memory(&plain).unwrap().to_rgb8();
        assert_eq!(a.into_raw(), b.into_raw());
        // And the lossless optimizer passes them through verbatim.
        let opt = optimize_jpeg(&j).unwrap();
        let osegs = segments(&opt);
        assert!(osegs.iter().any(|s| s.0 == 0xE1 && s.1[6..] == exif[..]));
        assert!(osegs.iter().any(|s| s.0 == 0xE2 && s.1[14..] == icc[..]));
    }

    #[test]
    fn large_icc_is_chunked() {
        let rgb = test_rgb(16, 16);
        let icc: Vec<u8> = (0..150_000u32).map(|i| (i * 7) as u8).collect();
        let j = Encoder::new(80).icc_profile(&icc).encode_rgb(&rgb, 16, 16);
        let segs = segments(&j);
        let chunks: Vec<_> = segs.iter().filter(|s| s.0 == 0xE2).collect();
        assert_eq!(chunks.len(), 3);
        let mut got = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(&c.1[..12], b"ICC_PROFILE\0");
            assert_eq!(c.1[12] as usize, i + 1);
            assert_eq!(c.1[13] as usize, 3);
            got.extend_from_slice(&c.1[14..]);
        }
        assert_eq!(got, icc);
        assert!(image::load_from_memory(&j).is_ok());
    }

    #[test]
    fn oversized_exif_is_skipped() {
        let rgb = test_rgb(16, 16);
        let big = vec![0u8; 70_000];
        let j = Encoder::new(80).exif(&big).encode_gray(
            &rgb.chunks(3).map(|p| p[0]).collect::<Vec<_>>(),
            16,
            16,
        );
        assert!(!segments(&j).iter().any(|s| s.0 == 0xE1));
        assert!(image::load_from_memory(&j).is_ok());
    }

    /// Every Huffman symbol a file defines must be codable in baseline
    /// 8-bit JPEG: DC sizes up to 11, AC sizes up to 10 (T.81 F.1.2).
    /// Strict decoders (macOS ImageIO) reject the whole file otherwise.
    fn assert_symbols_in_range(j: &[u8], what: &str) {
        for (m, p) in segments(j) {
            if m != 0xC4 {
                continue;
            }
            let mut i = 0;
            while i + 17 <= p.len() {
                let class = p[i] >> 4;
                let n: usize = p[i + 1..i + 17].iter().map(|&c| c as usize).sum();
                for &sym in &p[i + 17..i + 17 + n] {
                    if class == 0 {
                        assert!(sym <= 11, "{what}: DC size {sym}");
                    } else {
                        assert!(sym & 0x0F <= 10, "{what}: AC symbol {sym:#04x}");
                    }
                }
                i += 17 + n;
            }
        }
    }

    #[test]
    fn hard_edges_at_high_quality_stay_baseline_codable() {
        // A QR-like pattern: 3-pixel black/white modules, the worst case
        // for coefficient magnitude (and for deringing overshoot).
        let (w, h) = (111u32, 111u32);
        let gray: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) / 3, (i / w) / 3);
                let v = (x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) >> 7;
                if v & 1 == 1 { 0 } else { 255 }
            })
            .collect();
        let rgb: Vec<u8> = gray.iter().flat_map(|&v| [v, v, v]).collect();
        let cmyk: Vec<u8> = gray.iter().flat_map(|&v| [0, 0, 0, 255 - v]).collect();
        for q in [90u8, 93, 95, 97, 100] {
            for preset in [QuantPreset::AnnexK, QuantPreset::Photo] {
                for prog in [false, true] {
                    let mk = || Encoder::new(q).quant_preset(preset).progressive(prog);
                    let tag = format!("q{q} {preset:?} prog={prog}");
                    let g = mk().encode_gray(&gray, w, h);
                    assert_symbols_in_range(&g, &format!("gray {tag}"));
                    assert!(optimize_jpeg(&g).is_some(), "gray {tag}");
                    let c = mk().sampling(Sampling::S444).encode_rgb(&rgb, w, h);
                    assert_symbols_in_range(&c, &format!("rgb {tag}"));
                    assert!(optimize_jpeg(&c).is_some(), "rgb {tag}");
                    for adobe in [None, Some(AdobeTransform::Ycck)] {
                        let k = mk().encode_cmyk(&cmyk, w, h, adobe);
                        assert_symbols_in_range(&k, &format!("cmyk {adobe:?} {tag}"));
                        assert!(decode_cmyk(&k).is_some(), "cmyk {adobe:?} {tag}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;

    /// The gradient energy is one running sum over the luma blocks in
    /// order, term by term, whatever the number of bands: here against
    /// that sum made block by block the plain way.
    #[test]
    fn gradient_energy_is_the_running_sum_over_blocks() {
        let (w, h) = (320u32, 256u32); // whole blocks, enough for 3 workers
        let rgb = image(w, h);
        let mut plain = Component::with_grid(40, 40, 32, 1, 1, 0, 40 * 32);
        plain.energy = true;
        for by in 0..32 {
            for bx in 0..40 {
                let mut px = [0f32; 64];
                for (i, v) in px.iter_mut().enumerate() {
                    let p = &rgb[((by * 8 + i / 8) * w as usize + bx * 8 + i % 8) * 3..][..3];
                    let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                    *v = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
                }
                plain.push_block(&mut px, &[1.0; 64]);
            }
        }
        let want = (plain.e_low.to_bits(), plain.e_all.to_bits());
        assert!(plain.e_all > 0.0);
        let energy = |threads: usize, one_band: bool| {
            LAST_ENERGY.with(|e| e.set((0.0, 0.0)));
            FORCE_ONE_BAND.with(|f| f.set(one_band));
            Encoder::new(90)
                .sampling(Sampling::S444)
                .deringing(false)
                .threads(threads)
                .encode_rgb(&rgb, w, h);
            FORCE_ONE_BAND.with(|f| f.set(false));
            let (low, all) = LAST_ENERGY.with(|e| e.get());
            (low.to_bits(), all.to_bits())
        };
        assert_eq!(energy(1, false), want, "one band");
        assert_eq!(energy(3, true), want, "one band after several");
        // Several bands may round differently, but not by more than that.
        let (low, all) = energy(3, false);
        let near = |a: u64, b: u64| (f64::from_bits(a) / f64::from_bits(b) - 1.0).abs() < 1e-9;
        assert!(near(low, want.0) && near(all, want.1));
    }

    /// Gradient + xorshift noise with flat panels, at odd sizes so MCU
    /// padding and uneven bands are exercised.
    fn image(w: u32, h: u32) -> Vec<u8> {
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let n = (s >> 58) as u8;
                if (x / 40 + y / 30) % 3 == 0 {
                    px.extend_from_slice(&[230, 232, 235]);
                } else {
                    px.extend_from_slice(&[
                        (x % 256) as u8 ^ n,
                        (y % 256) as u8,
                        n.wrapping_mul(4),
                    ]);
                }
            }
        }
        px
    }

    #[test]
    fn output_is_identical_for_every_thread_count() {
        // The largest has work for many workers; small images use few.
        for (w, h) in [(1031u32, 701u32), (333, 251), (17, 9), (8, 300), (640, 16)] {
            let rgb = image(w, h);
            for sampling in [Sampling::S420, Sampling::S444] {
                for progressive in [true, false] {
                    for quality in [50u8, 93] {
                        let enc = |t: usize| {
                            Encoder::new(quality)
                                .sampling(sampling)
                                .progressive(progressive)
                                .threads(t)
                                .encode_rgb(&rgb, w, h)
                        };
                        let one = enc(1);
                        assert!(decode_jpeg(&one).is_some(), "{w}x{h} undecodable");
                        for t in [2, 3, 8, 0, usize::MAX] {
                            assert!(
                                one == enc(t),
                                "{w}x{h} {sampling:?} progressive={progressive} q{quality} differs at {t} threads"
                            );
                        }
                        // The second pass an energy at the threshold takes.
                        FORCE_ONE_BAND.with(|f| f.set(true));
                        let again = enc(3);
                        FORCE_ONE_BAND.with(|f| f.set(false));
                        assert!(one == again, "{w}x{h} differs after the one-band pass");
                    }
                }
            }
        }
    }
}
