# Changelog

Notable changes per version. 0.13.0 is the first published release; the
entries before it record development that preceded publication.

## Unreleased

## 0.17.0

New in the npm package: `smallpeg/jxl`, the experimental JPEG to JPEG XL
conversion, with a worker entry point of its own. The `jxl` feature's
functions return a `JxlOutput` (a breaking change within the experimental
feature), keep the reconstruction data for more grayscale files, and
take less than half the memory. The encoder's and the re-coding
functions' output is unchanged, and so is the JPEG XL output apart from
those grayscale files. The npm package was last released at 0.15.0;
nothing of 0.16.0 changed it.

- `jxl` feature (experimental): `jpeg_to_jxl` takes less than half the
  memory: about 6 to 21 bytes per pixel besides the JPEG and the result,
  where the worst case was near 50. The tokens it holds before coding
  are packed into four bytes each, and they and the finished sections
  are let go as they are used. What a file holds besides the image
  (comments, application segments) is copied once into the
  reconstruction data and once into the result: a file that is mostly
  such data takes about three times its size, down from about six, and
  a result in a container is held twice for a moment at the end. The
  output is unchanged.
- npm: new entry points `smallpeg/jxl` and `smallpeg/jxl/web` with
  `toJxl(jpeg, options?)`, the experimental JPEG to JPEG XL conversion.
  It returns `{ data, reconstruction }`, or `undefined` for a file it
  cannot convert, which includes frames over 100 megapixels and files
  over 512 MB. They share a `.wasm` file apart from the main one, so the
  main entry points load no more than before. A third,
  `smallpeg/jxl/worker`, runs the conversion in a Web Worker:
  its `createWorker()` gives `toJxl`, `ready` and `terminate()`, and the
  worker of `smallpeg/worker` stays as it is. Experimental: may change
  in any release.
- `jxl` feature (experimental): `jpeg_to_jxl` and `jpeg_to_jxl_with`
  return a `JxlOutput` with the file in `data` and, in `reconstruction`,
  whether it holds what rebuilds the original JPEG byte for byte. They
  returned the bytes alone, which did not say when that data had to be
  left out.
- `jxl` feature (experimental): a grayscale JPEG whose frame header
  gives sampling factors of 2x1, 1x2 or 2x2 now gets the data that
  rebuilds it, as files with 1x1 did. Such headers are common (one
  widely used encoder writes 2x1 for grayscale), and those files were
  converted as images only.

## 0.16.0

New: `Encoder::threads`, more of the encoder on worker threads with the
`parallel` feature, and an experimental `jxl` feature (JPEG to JPEG XL).
The encoder's and the re-coding functions' output is unchanged, with any
features and any number of threads. The npm package is not released at
this version: nothing in it changed, and it stays at 0.15.0.

- `parallel` feature: color conversion and the DCT, the symbol
  statistics, the DC trellis and the progressive scan search and output
  now run on worker threads, as trellis quantization already did. A
  2048x1536 photo on 12 cores: progressive 42 ms to 20 ms, baseline
  25 ms to 14 ms. The output is byte-identical for any number of
  threads. `optimize_jpeg` and `upright_jpeg` use the threads too where
  they write progressive output, with the default number of them. Small
  images use fewer workers, one per 1024 blocks, so the smallest stay on
  the calling thread. (Contributed by Vulptik, #22.)
- `Encoder::threads(n)` sets the number of worker threads for the
  `parallel` feature: 0, the default, uses the available cores up to 8;
  counts above 64 are taken as 64. Without the feature everything runs
  on the calling thread, as before.
- Experimental `jxl` feature: `jpeg_to_jxl` recompresses a JPEG, color or
  grayscale, as JPEG XL without changing its coefficients, so the image
  is the same and a photo's file about 10% smaller (less for very small
  files). The output also holds what a JPEG XL decoder needs to write
  the original JPEG file back byte for byte, whenever the format can
  describe the file; `JxlOptions::reconstruction(false)` leaves that out
  (a few hundred bytes for most files). The color profile, Exif and XMP
  carry over. CMYK and RGB-coded JPEGs are not converted. It takes about
  20 to 50 bytes of memory per pixel. The feature is outside the
  compatibility promise: its functions and output may change in any
  release. It is not in the npm package.
- Website: photos with colors outside sRGB, such as Display P3 photos
  from an iPhone, keep those colors when re-encoded. They were read
  through an sRGB canvas, which dulled the most saturated colors; they
  are now read in Display P3 and the output carries a Display P3
  profile. Other images are encoded as before.
- Website: HDR photos are previewed at standard brightness, so the
  original and the output are compared on equal terms.

## 0.15.0

New: `upright_jpeg` (npm: `upright`) and the npm `smallpeg/worker` entry.
The encoder's output is unchanged. Re-coding output changes only where
noted below (late APPn/COM segments stay in place). Files with an invalid
progressive scan order are now refused. The npm package needs WebAssembly
SIMD (Safari 16.4 or later).

- Fix: re-coding (`optimize_jpeg`, `upright_jpeg`, and the encoder) could
  panic, and in WebAssembly abort the instance, on symbol counts so
  skewed that the optimal Huffman code needed lengths over 32 (millions
  of symbols in a Fibonacci-like distribution, so only from a crafted
  file). The counts are now halved until the code fits.
- Fix: progressive scans must follow the successive-approximation order
  (one first scan per coefficient, then refinements lowering Al by one).
  A file repeating a refinement scan could make every scan revisit every
  block while reading almost nothing: a 200 KB file took 4.5 seconds,
  and is now refused at once. A coefficient is now coded in at most 14
  scans, so a frame's decode time stays within a few times what its
  pixel count implies. Valid files decode as before; files with an
  invalid progression, which some decoders accept with a warning, are
  refused.
- npm: `resize` takes new sizes up to 65535 per side, as for a JPEG, and
  refuses a new width that, times the old height, is over 100
  megapixels. The filter keeps a table per output column and row and a
  working image of that size, so until now a small image resized to a
  very long, thin or wide shape could run the WebAssembly out of memory
  or overflow its 32-bit sizes instead of throwing.
- Website: a Content-Security-Policy limits the page to its own scripts,
  styles and workers.
- npm: `smallpeg/worker` runs the functions in a Web Worker.
  `createWorker()` returns the same functions returning promises, plus
  `ready` and `terminate()`; errors arrive as the same classes, and
  returned bytes and image data are moved back without a copy. After a
  WebAssembly error (a trap, such as running out of memory) the worker
  stops and rejects later calls, instead of reusing a module that may be
  in an inconsistent state.
- `upright_jpeg` (npm: `upright`) turns a JPEG upright by its EXIF
  orientation without decoding it to pixels: blocks of quantized
  coefficients are moved, mirrored and transposed, so no quality is lost,
  and the tag is set to 1. An axis that is mirrored is trimmed to whole
  MCUs (up to 15 pixels in most files) when it is not already.
- Fix: `optimize_jpeg` (and the other re-coding functions) moved APPn and
  COM segments that came after the frame header to the front of the file.
  An EXIF orientation there, which header readers never saw, then took
  effect and turned the image. Such segments now stay after the frame
  header, or after the scans when they followed the first scan.
- npm package: the WebAssembly build uses 128-bit SIMD. Decoding is about
  a third faster and resizing about 6% faster; the output is
  byte-identical. Browsers need SIMD support: Safari (and iOS) 16.4 or
  later, from March 2023; Chrome and Firefox have had it since 2021.

## 0.14.0

The Rust API is unchanged, and so is the encoder's output. The npm
package's JavaScript API changes (breaking); see below.

- Progressive encoding is about 1.6x faster (an 8-megapixel photo at
  quality 80: 335 ms to 210 ms, one thread). The output is byte-identical:
  the scan search's statistics are reused when writing, the baseline
  candidate is only written when it can still be smaller, band statistics
  take one pass, and refinement scans no longer allocate per block.
- npm package: a TypeScript API replaces the generated bindings
  (breaking). Images are plain `{ data, width, height, channels }`
  objects that chain from one call to the next, `channels` is inferred
  from the data length when left out, so a canvas `ImageData` encodes as
  is, and options go in an object (`encode(image, { quality })`,
  `resize(image, w, h, { linear })`, `crop(image, { x, y, width, height })`).
  Results are plain objects with precise types (no `free()`), returned
  bytes are typed `Uint8Array<ArrayBuffer>` so they work as a `BlobPart`
  (the types need TypeScript 5.7 or later), and `fitWithin` returns a
  `[width, height]` tuple. Sizes, coordinates and `quality` must be whole
  numbers in range and throw an `Error` otherwise (a fractional size used
  to be truncated, and `quality` above 255 wrapped around). Arguments of
  the wrong kind throw a `TypeError`: JPEG bytes must be a `Uint8Array`,
  pixel data a `Uint8Array` or `Uint8ClampedArray`, options an object and
  flags booleans. Images have 1, 3 or 4 channels (`crop` used to take any
  count and `applyOrientation` 1 to 4). An orientation must be a number
  (a string such as `"6"` used to be converted); one outside 1 to 8
  leaves the pixels as they are. `resize` refuses results over 100
  megapixels with a `RangeError` instead of running out of memory.
- npm package: the browser build exports `init` by name (still the
  default export too) and throws a clear error when used before it. The
  Node build has the same `init`, `initSync` and default export as no-ops,
  so code shared between the two runs unchanged. The 0.13 init forms,
  `init({ module_or_path })` and `initSync({ module })`, still work. The
  package is now ES modules; `require()` needs Node 22.12 or later.
- npm package: built with wasm-bindgen 0.2.127. Decoded pixels are moved
  out of WebAssembly instead of copied twice.
- Documentation: the README and the npm README are short (install,
  example, features, links), and the details moved to `docs/`: a guide,
  color profiles, how the encoder works, and the JavaScript API.

## 0.13.1

No code changes; the encoder's output is identical to 0.13.0.

- README rewritten for users: install, features, usage. The status list
  and roadmap, which were development notes, are gone.
- `homepage` points at the landing page, https://sawapi.github.io/smallpeg/.
- npm package: requires Node 22 or later (18 and 20 are end of life).
- `#![forbid(unsafe_code)]`: the crate already used no `unsafe`; now the
  compiler enforces it.
- Releases are published from GitHub Actions with trusted publishing, so
  no registry token is stored. npm versions are staged by the workflow and
  go live after a maintainer approves them with 2FA.

## 0.13.0

- npm package `smallpeg`: WebAssembly builds for Node and for browsers
  and bundlers, built from the bindings crate in `wasm/` (the Rust crate
  itself still has no dependencies). Encode, optimize, strip metadata,
  decode, read the header, orientation, crop, resize, fit.
- `crop` (with `CropRect`): copies a rectangle out of interleaved pixels
  with any channel count; `None` if the rectangle is empty or not inside
  the image.
- `resize_rgb_linear` / `resize_gray_linear`: Lanczos3 in linear light
  (sRGB decode, filter, encode). Filtering the stored codes, as
  `resize_rgb` does, darkens fine light-and-dark detail: a black/white
  pixel checkerboard reduced 4x comes out at 128 instead of the 188 that
  matches its brightness. About a quarter more time than `resize_rgb`.
- `fit_within`: aspect-preserving target size for a bounding box; never
  enlarges.
- 4:4:4 with the default `Photo` tables now doubles the chroma quant
  steps (fading back to 1x between quality 95 and 100). The shared fine
  table overspent on full-resolution chroma: on photos held out from the
  choice of factor (CLIC 2020 mobile, Kodak) output is 5-10% smaller at
  equal SSIMULACRA2 and 9-14% at equal butteraugli; rendered documents
  neutral to slightly better. 4:2:0 and `AnnexK` output is byte-identical to before.
- RGB encoding with the default `Photo` tables now weights the trellis
  distortion mildly by frequency (squared step-unit error times
  (step / geometric-mean AC step)^0.5, so coarse-step high frequencies
  are a little dearer to drop) and runs the chroma trellis at the luma
  lambda instead of half of it. Chosen on CLIC 2020 professional; on
  images held out from that choice (CLIC mobile, Kodak, rendered
  documents) output at equal SSIMULACRA2 / butteraugli is 0-2% smaller
  at 4:2:0 (neutral at high quality on SSIMULACRA2) and 2-3.5% smaller
  at 4:4:4, worst single image +1.4% (+2.5% for one document page at
  4:4:4), at the same or slightly lower encode time. At a given quality
  setting files come out a few percent smaller with a correspondingly
  small quality change. `AnnexK`, grayscale and CMYK output is
  byte-identical to before.
- `strip_metadata`: lossless metadata removal (EXIF with GPS and camera
  data, XMP, IPTC, comments, other APPn, data appended after EOI) that
  keeps what decoders need for pixels and color (JFIF, ICC profile, Adobe
  APP14, tables, scans byte-for-byte). A non-1 EXIF orientation is
  rewritten as a 26-byte EXIF holding only that tag, so phone photos stay
  upright. On iPhone photos it removed about 10 KB each with identical
  decoded pixels.
- EXIF orientation: `JpegInfo::orientation` (1..=8; matches an
  independent EXIF reader on phone photos) and `apply_orientation`, which turns
  decoded pixels (any channel count) upright for re-encoding. The
  decoders keep returning pixels as stored (right for PDFs, whose viewers
  ignore EXIF).
- Hostile-input hardening (from a security review of the decode paths):
  - `jpeg_info`: frame-header probe (dimensions, component count,
    progressive) that decodes and allocates nothing, so callers can check
    a size budget before decoding.
  - `Limits` with `decode_jpeg_with`, `decode_cmyk_with`,
    `optimize_jpeg_with` and `optimize_jpeg_baseline_with`. `max_pixels`
    is checked at the frame header, before any allocation: memory is set
    by the header alone (a 1 KB file could declare a frame that takes
    over 2 GB to decode), and on WASM linear memory never shrinks back.
  - Behavior change: the plain functions (`decode_jpeg`, `decode_cmyk`,
    `optimize_jpeg`, `optimize_jpeg_baseline`) now refuse frames above
    100 megapixels by default; pass `Limits::default().max_pixels(..)` to
    the `_with` variants for larger ones.
  - At most 128 scans per file by default (`max_scans`). Each scan visits
    every block of its component however few bytes it has, so many tiny
    scans made decode time grow with the scan count (177 KB taking 3.3 s
    per call). Across 10,303 local JPEGs the most scans was 14.
  - Progressive AC refinement refuses a coefficient that would overflow
    i16 (only reachable through inconsistent successive-approximation
    scans) instead of wrapping; it panicked in builds with overflow
    checks.
  - The fuzz target now also covers `jpeg_info`, `decode_jpeg` and
    `decode_cmyk`.
- Fix: at high quality (any quantization step of 1, e.g. AnnexK from about
  q93) clipped-pixel deringing could push coefficients past baseline
  range: AC size 11 (strict decoders such as macOS ImageIO reject the
  file) and DC beyond 1023 (ImageIO decodes the image uniformly darker).
  Deringing is now off whenever a step is 1, its overshoot is capped,
  and every coefficient is clamped to the codable range in all encode
  paths. A regression test checks the DHT symbol sizes and the parsed
  coefficient range on hard-edged content at q90–100.
- `Encoder::deringing(bool)` (stable): turn clipped-pixel deringing off.
  On images that are themselves decoded JPEGs (30 RGB/gray images from
  real PDFs, downsampled and re-encoded) it measured neutral at about 25% more
  encode time.
- `decode_jpeg`: grayscale and color JPEG decoding to 8-bit gray or RGB
  (`DecodedImage`), the conventional color-space rules and interpolating
  chroma upsampling.
- `decode_cmyk` upsamples subsampled components by interpolation instead
  of replication (now within ±2 of an independent decoder's output).
- Encoding 7–15% faster, output byte-identical: the symbol walker visits
  only nonzero AC positions (bitmask + trailing zeros), the bit writer
  emits 32-bit words with a word-level 0xFF test for stuffing, and the
  per-block energy split is accumulated only when the gradient check
  will read it (Photo preset above q60).
- Decoding about 1.75x faster (373 PDF JPEGs: 195 -> 110 ms), same
  pixels to within rounding: row-wise
  fixed-point YCbCr->RGB that vectorizes (no table gathers), 2x
  upsampling with the edges peeled off so the interior has no index
  checks, and a min/max fold for the coefficient range check. Tried and
  dropped after measuring: an "axpy" IDCT and a combined AC
  code-plus-magnitude lookup table (both slower here).
- Entropy decoding resolves Huffman codes of up to 9 bits with one table
  lookup, which also speeds up `optimize_jpeg`.
- `Encoder::cmyk_dering` (hidden, tuning only): deringing on CMYK
  channels, off by default — neutral on CMYK images from real PDFs.
- CMYK support without color conversion: `decode_cmyk` (four-component
  baseline/progressive JPEG to stored C, M, Y, K values plus the Adobe
  APP14 coding; YCCK is un-transformed, nothing is inverted),
  `resize_cmyk`, and `Encoder::encode_cmyk`, which writes
  its own APP14 with the transform byte matching the coding performed
  (plain CMYK, or YCCK with chroma subsampling per `Encoder::sampling`).
  Four-component output carries no JFIF segment.
- `Sampling` and `QuantPreset` derive `Debug`.
- Clipped-pixel deringing, on by default: luma pixels at full white in
  mixed blocks are encoded as a minimum-curvature overshoot past 255 (cap
  48, up to 10 projected-SOR sweeps), so quantization ripple is clamped
  away by the decoder. Rendered documents: 17–19% smaller at equal
  SSIMULACRA2, 10–13% at equal butteraugli; photographs neutral. Encode
  time on text-heavy pages rises ~40%. Output bytes change for any image
  with white areas. `Encoder::dering` (hidden, tuning only) adjusts or
  disables it.
- Documentation: a "Color profiles" section (README and crate docs) on
  forwarding the source ICC profile, since the encoder cannot infer the
  pixels' color space.

## 0.12.0

- The default quantization preset takes its final name,
  `QuantPreset::Photo`. Behavior unchanged.
- Public documentation describes the encoder on its own terms, without naming
  other tools; provenance for the borrowed numeric table stays in
  `THIRD_PARTY.md`.
- Release engineering: clippy-clean at `-D warnings`, crate metadata and a
  scoped `include` list, CI (fmt / clippy / test / wasm / packaging / docs /
  MSRV), and a `cargo-fuzz` target for `optimize_jpeg` running weekly.
- Declared MSRV: Rust 1.88.
- Crate-level documentation rewritten for the current feature set, `# Panics`
  sections on the entry points that assert on their arguments, and a
  doc-tested usage example.

## 0.11.0

- ICC profile and EXIF embedding for fresh encodes: `Encoder::icc_profile`
  (APP2, chunked per the ICC spec above 64 KB) and `Encoder::exif` (APP1).
  Both are stable API. `optimize_jpeg` already preserved these segments.

## 0.10.1

- `optimize_jpeg_baseline`: same lossless path, but the output is always
  baseline sequential, for targets that cannot take progressive files.
  Progressive input is converted losslessly. Stable API.

## 0.10.0

- `optimize_jpeg`: lossless re-optimization of an existing JPEG. The file is
  entropy-decoded to its quantized coefficients (Huffman layer only — no
  IDCT, no color conversion) and re-emitted through the optimized-Huffman and
  scan-optimized emitters; the smaller rendering wins. Pixels, quantization
  tables, sampling and APPn/COM metadata are preserved exactly. Returns
  `None` for anything that cannot be round-tripped safely.

## 0.9.1

- Progressive scan search and emission walk a CSR sparse view of the
  coefficients: 2.35x on the A4 scan, byte-identical output.

## 0.9.0

- DC trellis: the per-component DPCM chains get a Viterbi pass over
  {round−1, round, round+1}. Worth 1–2.5% on smooth and document content at
  equal quality; neutral on photographs.

## 0.8.0

- Quality-adaptive luma table shape, gated per image by the
  quant-independent DCT energy split. The alternative — masking-based
  per-block deadzone — was implemented, measured strictly worse under
  SSIMULACRA2, and rejected.

## 0.7.0

- The pixel pipeline is streamed one MCU row at a time and the pre-rounding
  coefficients are freed once the trellis outcome is final: peak memory down
  to ~10 bytes/pixel.

## 0.6.1

- Per-image trellis lambda for gradient-dominant content. An explicit
  `trellis_lambda` disables the gate.

## 0.6.0

- Quantization table presets (`QuantPreset`), with the fine-grained photo
  table as the new default; the classic T.81 Annex K tables stay available
  and remain the better pick for documents and scans at high quality.
- Chroma trellis strength follows the preset.

## 0.5.0

- Progressive mode with per-image scan optimization, on by default: band
  split points, point-transform depth and DC precision are priced with real
  statistics passes, and both progressive and baseline streams are rendered
  so the smaller one ships.

## 0.4.0

- Zero-dependency Lanczos3 resampling: `resize_rgb` / `resize_gray`.

## 0.3.0

- AAN scaled DCT with descaling folded into the quantization divisors,
  verified bit-for-bit against the naive orthonormal transform.
- Trellis DP walks only nonzero candidate positions; output unchanged.

## 0.2.0

- Trellis quantization, on by default: every AC coefficient is re-decided by
  dynamic programming against the real run-length/EOB structure.
- Chroma runs at a much smaller lambda than luma.

## 0.1.0

- From-scratch baseline JFIF encoder: color transform, DCT, quantization and
  Huffman coding written from ITU-T T.81, with per-image optimized Huffman
  tables always on. Grayscale and RGB, 4:4:4 and 4:2:0. No dependencies.
