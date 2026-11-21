# Guide

Common tasks with the Rust API. Every function is documented on
[docs.rs](https://docs.rs/smallpeg); the JavaScript equivalents are in
[npm-api.md](npm-api.md).

## Encoding

```rust
use smallpeg::{Encoder, QuantPreset, Sampling};

let jpeg = Encoder::new(85)                // quality 1..=100
    .sampling(Sampling::S420)              // or S444: full-resolution chroma
    .quant_preset(QuantPreset::Photo)      // default; AnnexK suits scans and documents
    .progressive(true)                     // default; the smaller of progressive and
                                           // baseline is kept
    .encode_rgb(&rgb, width, height);

let gray = Encoder::new(85).encode_gray(&luma, width, height);
```

`encode_rgb` takes `width * height * 3` bytes, `encode_gray` one byte per
pixel. A buffer whose length does not match, or a width or height of 0 or
above 65535, panics.

## Resizing and cropping

Reducing before encoding is usually the biggest saving. `fit_within`
computes the largest size inside a box, keeping the aspect ratio and never
enlarging.

```rust
// Crop, then reduce to fit 1600x1600.
let rect = smallpeg::CropRect { x: 100, y: 50, width: 3000, height: 2000 };
if let Some(cut) = smallpeg::crop(&rgb, width, height, 3, rect) {
    let (w, h) = smallpeg::fit_within(rect.width, rect.height, 1600, 1600);
    let small = smallpeg::resize_rgb_linear(&cut, rect.width, rect.height, w, h);
    let jpeg = smallpeg::Encoder::new(80).encode_rgb(&small, w, h);
}
```

Resizing uses a Lanczos3 filter: `resize_rgb`, `resize_gray` and
`resize_cmyk`. The `_linear` variants (`resize_rgb_linear`,
`resize_gray_linear`) filter in linear light, which keeps fine light and
dark detail, such as text or foliage, from darkening when reduced. They
assume sRGB input.

## Orientation

Phone photos are usually stored sideways with an EXIF orientation that
tells viewers how to turn them. `upright_jpeg` turns the JPEG itself
without decoding it to pixels, so there is no loss of quality, and sets the tag to
1, for viewers that ignore EXIF:

```rust
if let Some(up) = smallpeg::upright_jpeg(&jpeg) {
    // same pixels, upright; the entropy coding is optimized too
}
```

The result may be progressive; `optimize_jpeg_baseline` on it gives
baseline output, also without loss.

It moves whole blocks of the compressed image, so an axis it mirrors must
be a whole number of MCUs (8 or 16 pixels in most files). Otherwise the
last partial row or column (up to 15 pixels in most files) is cut off: a
5712 x 4284 photo with orientation 6 comes out 4272 x 5712. An image
smaller than one MCU on such an axis returns `None`.

Decoders return pixels as stored. To turn decoded pixels instead:

```rust
if let (Some(info), Some(img)) = (smallpeg::jpeg_info(&jpeg), smallpeg::decode_jpeg(&jpeg)) {
    let (pixels, w, h) = smallpeg::apply_orientation(
        &img.pixels, img.width, img.height, img.channels as usize, info.orientation,
    );
}
```

## Optimizing an existing JPEG

`optimize_jpeg` re-codes a baseline or progressive JPEG with bit-identical
pixels, keeping its quantization tables, sampling and metadata. An already
optimized file can come out larger, so compare lengths.

```rust
if let Some(opt) = smallpeg::optimize_jpeg(&input) {
    if opt.len() < input.len() {
        // same pixels, fewer bytes
    }
}
```

`optimize_jpeg_baseline` always writes baseline sequential output, for
targets that do not accept progressive JPEGs (PDF embedding, for example);
a progressive input is converted losslessly.

## Metadata

`strip_metadata` removes EXIF (including GPS position), XMP, comments and
data after the image, without touching the image data. It keeps the ICC
profile and the orientation. To add metadata to new output, use
`Encoder::icc_profile` and `Encoder::exif`; see
[color-profiles.md](color-profiles.md) for why forwarding the profile
matters.

## CMYK

CMYK and YCCK JPEGs (Adobe APP14) are decoded to their stored values and
re-encoded without an RGB round trip:

```rust
if let Some(img) = smallpeg::decode_cmyk(&cmyk_jpeg) {
    let (w, h) = (img.width / 2, img.height / 2);
    let small = smallpeg::resize_cmyk(&img.pixels, img.width, img.height, w, h);
    let jpeg = smallpeg::Encoder::new(85).encode_cmyk(&small, w, h, img.adobe);
}
```

## Untrusted input

The decoders and `optimize_jpeg` take bytes from anywhere and never panic:
they return `None` for anything they cannot handle safely. Memory follows
the frame size declared in the header, not the file size, so for input you
do not control, check `jpeg_info` first or pass a `Limits` to the `_with`
variants (`decode_jpeg_with`, `optimize_jpeg_with`, `upright_jpeg_with`, ...). `Limits` caps the
frame's pixel count (100 megapixels by default) and the number of
progressive scans. `optimize_jpeg`, which shares the decoders' parsing, is fuzzed weekly
in CI.

## Threads

The optional `parallel` feature spreads encoding across cores with
`std::thread` (no extra dependencies): color conversion and DCT, trellis
quantization, and the progressive scan search and output. `Encoder::threads`
sets the number of threads (by default the available cores, up to 8; at
most 64). Small images use fewer, where threads would not pay off. The
output is byte-identical for any thread count. It is not available on
`wasm32-unknown-unknown`.

```toml
smallpeg = { version = "0.15", features = ["parallel"] }
```
