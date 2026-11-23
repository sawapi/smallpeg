//! The package's main entry points: encode, re-code, decode, resize.

use wasm_bindgen::prelude::*;

const DEFAULT_QUALITY: u8 = 80;

/// Decoded or transformed pixels. The TypeScript layer reads the fields,
/// moves the pixels out with `take_pixels`, and frees it.
#[wasm_bindgen]
pub struct Image {
    width: u32,
    height: u32,
    channels: u8,
    pixels: Vec<u8>,
}

#[wasm_bindgen]
impl Image {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[wasm_bindgen(getter)]
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Moves the pixels out (no copy on the Rust side); a second call
    /// returns an empty array.
    pub fn take_pixels(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pixels)
    }
}

/// Frame-header facts, returned by `info`.
#[wasm_bindgen]
pub struct JpegInfo {
    width: u32,
    height: u32,
    components: u8,
    progressive: bool,
    orientation: u8,
}

#[wasm_bindgen]
impl JpegInfo {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[wasm_bindgen(getter)]
    pub fn components(&self) -> u8 {
        self.components
    }

    #[wasm_bindgen(getter)]
    pub fn progressive(&self) -> bool {
        self.progressive
    }

    #[wasm_bindgen(getter)]
    pub fn orientation(&self) -> u8 {
        self.orientation
    }
}

fn check_pixels(len: usize, width: u32, height: u32, channels: u32) -> Result<(), JsError> {
    if width == 0 || height == 0 {
        return Err(JsError::new(
            "smallpeg: width and height must be at least 1",
        ));
    }
    let want = width as u64 * height as u64 * channels as u64;
    if len as u64 != want {
        return Err(JsError::new(&format!(
            "smallpeg: expected {want} bytes for {width}x{height} with {channels} channel(s), got {len}"
        )));
    }
    Ok(())
}

fn check_channels(channels: u32) -> Result<(), JsError> {
    if matches!(channels, 1 | 3 | 4) {
        Ok(())
    } else {
        Err(JsError::new("smallpeg: channels must be 1, 3 or 4"))
    }
}

/// Encode pixels; 4 channels are RGBA, flattened onto white.
#[wasm_bindgen]
pub fn encode(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u32,
    quality: Option<u8>,
    full_color: Option<bool>,
) -> Result<Vec<u8>, JsError> {
    check_channels(channels)?;
    check_pixels(pixels.len(), width, height, channels)?;
    if width > 65535 || height > 65535 {
        return Err(JsError::new(
            "smallpeg: JPEG dimensions are limited to 65535",
        ));
    }
    let sampling = if full_color.unwrap_or(false) {
        smallpeg::Sampling::S444
    } else {
        smallpeg::Sampling::S420
    };
    let enc = smallpeg::Encoder::new(quality.unwrap_or(DEFAULT_QUALITY)).sampling(sampling);
    Ok(match channels {
        1 => enc.encode_gray(pixels, width, height),
        3 => enc.encode_rgb(pixels, width, height),
        _ => {
            let mut rgb = Vec::with_capacity(pixels.len() / 4 * 3);
            for p in pixels.as_chunks::<4>().0 {
                let a = p[3] as u32;
                for &c in &p[..3] {
                    rgb.push(((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8);
                }
            }
            enc.encode_rgb(&rgb, width, height)
        }
    })
}

/// Lossless re-coding; `baseline` forces sequential output.
#[wasm_bindgen]
pub fn optimize(jpeg: &[u8], baseline: bool) -> Option<Vec<u8>> {
    if baseline {
        smallpeg::optimize_jpeg_baseline(jpeg)
    } else {
        smallpeg::optimize_jpeg(jpeg)
    }
}

#[wasm_bindgen]
pub fn upright(jpeg: &[u8]) -> Option<Vec<u8>> {
    smallpeg::upright_jpeg(jpeg)
}

#[wasm_bindgen]
pub fn strip_metadata(jpeg: &[u8]) -> Option<Vec<u8>> {
    smallpeg::strip_metadata(jpeg)
}

#[wasm_bindgen]
pub fn info(jpeg: &[u8]) -> Option<JpegInfo> {
    smallpeg::jpeg_info(jpeg).map(|i| JpegInfo {
        width: i.width,
        height: i.height,
        components: i.components,
        progressive: i.progressive,
        orientation: i.orientation,
    })
}

#[wasm_bindgen]
pub fn decode(jpeg: &[u8]) -> Option<Image> {
    smallpeg::decode_jpeg(jpeg).map(|d| Image {
        width: d.width,
        height: d.height,
        channels: d.channels,
        pixels: d.pixels,
    })
}

#[wasm_bindgen]
pub fn apply_orientation(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u32,
    orientation: u8,
) -> Result<Image, JsError> {
    check_channels(channels)?;
    check_pixels(pixels.len(), width, height, channels)?;
    let (px, w, h) =
        smallpeg::apply_orientation(pixels, width, height, channels as usize, orientation);
    Ok(Image {
        width: w,
        height: h,
        channels: channels as u8,
        pixels: px,
    })
}

/// Largest resize result or working image, as for decoding.
const MAX_RESIZE_PIXELS: u64 = 100_000_000;

/// Lanczos3 resize; `linear` filters gray or RGB in linear light.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn resize(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u32,
    new_width: u32,
    new_height: u32,
    linear: bool,
) -> Result<Vec<u8>, JsError> {
    check_channels(channels)?;
    check_pixels(pixels.len(), width, height, channels)?;
    if new_width == 0 || new_height == 0 {
        return Err(JsError::new(
            "smallpeg: the new width and height must be at least 1",
        ));
    }
    // The filter keeps a table entry per output column and row, so cap each
    // side as for a JPEG, not only the area.
    if new_width > 65535 || new_height > 65535 {
        return Err(JsError::new(
            "smallpeg: the new width and height are limited to 65535",
        ));
    }
    // The filter's working image is new width by old height, then the
    // result: bounded like decoding, rather than left to run out of memory
    // (or overflow 32-bit sizes) and trap the instance.
    let work = new_width as u64 * new_height.max(height) as u64;
    if work > MAX_RESIZE_PIXELS {
        return Err(JsError::new(&format!(
            "smallpeg: resizing {width}x{height} to {new_width}x{new_height} needs over {} megapixels",
            MAX_RESIZE_PIXELS / 1_000_000
        )));
    }
    let (w, h, nw, nh) = (width, height, new_width, new_height);
    Ok(match (channels, linear) {
        (1, false) => smallpeg::resize_gray(pixels, w, h, nw, nh),
        (1, true) => smallpeg::resize_gray_linear(pixels, w, h, nw, nh),
        (3, false) => smallpeg::resize_rgb(pixels, w, h, nw, nh),
        (3, true) => smallpeg::resize_rgb_linear(pixels, w, h, nw, nh),
        (_, false) => smallpeg::resize_cmyk(pixels, w, h, nw, nh),
        (_, true) => {
            return Err(JsError::new(
                "smallpeg: linear resizing takes 1 or 3 channels",
            ));
        }
    })
}

/// `undefined` if the rectangle is empty or not entirely inside the image.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn crop(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u32,
    x: u32,
    y: u32,
    crop_width: u32,
    crop_height: u32,
) -> Result<Option<Vec<u8>>, JsError> {
    check_channels(channels)?;
    check_pixels(pixels.len(), width, height, channels)?;
    let rect = smallpeg::CropRect {
        x,
        y,
        width: crop_width,
        height: crop_height,
    };
    Ok(smallpeg::crop(
        pixels,
        width,
        height,
        channels as usize,
        rect,
    ))
}

/// `[width, height]` that fits, keeping the aspect ratio; never enlarges.
#[wasm_bindgen]
pub fn fit_within(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> Result<Vec<u32>, JsError> {
    if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
        return Err(JsError::new("smallpeg: all sizes must be at least 1"));
    }
    let (w, h) = smallpeg::fit_within(width, height, max_width, max_height);
    Ok(vec![w, h])
}
