# JavaScript API

The npm package `smallpeg`. TypeScript types are included, and every
function's documentation shows up in editors.

## Entry points

| Import | For |
|---|---|
| `"smallpeg"` | Node, and browsers through a bundler (resolves to the browser build there) |
| `"smallpeg/web"` | Browsers and bundlers, explicitly |
| `"smallpeg/worker"` | Browsers and bundlers: the same functions run in a Web Worker ([below](#in-a-web-worker)) |
| `"smallpeg/jxl"`, `"smallpeg/jxl/web"` | Experimental: JPEG to JPEG XL ([below](#experimental-jpeg-xl)), with a `.wasm` file of its own |

The first two export the same functions. The browser build has to load its `.wasm`
file before use:

```js
import { init, initSync, encode } from "smallpeg/web";

await init();            // fetches web/smallpeg_wasm_bg.wasm from the package
await init(url);         // or a URL, a Response, a fetch() promise, bytes, a WebAssembly.Module
initSync(bytes);         // synchronously, from bytes or a WebAssembly.Module (workers)
```

`init` can be called again harmlessly; a failed load can be retried.
Calling another function before the module has loaded throws an `Error`.

In Node the module loads on import. `init` and `initSync` exist there too
and do nothing, so code shared between Node and the browser can always
call them.

`require("smallpeg")` works in Node 22.12 or later. The package is ES
modules, so TypeScript code compiled to CommonJS needs `"module":
"nodenext"` and TypeScript 5.8 or later.

## Images

Pixels are passed and returned as plain objects:

```ts
interface Image {
  data: Uint8Array;   // width * height * channels bytes, interleaved, top row first
  width: number;
  height: number;
  channels: 1 | 3 | 4; // gray, RGB, RGBA (or any four-channel layout)
}
```

When you pass an image in, `data` can also be a `Uint8ClampedArray`, so a
canvas `ImageData` fits as is, and `channels` can be left out: it is worked
out from the length of `data`.

Returned bytes are fresh copies backed by their own `ArrayBuffer`, so they
can go straight into a `Blob`, and 4-channel pixels can be viewed as the
`Uint8ClampedArray` that `new ImageData` takes.

## `encode(image, options?)`

Encode pixels as a JPEG. 4 channels are RGBA: alpha is flattened onto
white.

- `quality`: integer 1 to 100, default 80.
- `fullColor`: keep chroma at full resolution (4:4:4) instead of halving
  it (4:2:0). Sharper colored text and lines, larger files. Default false.

## `optimize(jpeg, options?)`

Re-code an existing JPEG with identical pixels. Returns `undefined` if the
file is not one that can be re-coded. An already optimized file can come
out larger; compare lengths.

- `baseline`: write baseline sequential output instead of progressive,
  for targets that do not accept progressive JPEGs (PDF embedding, for
  example). Default false.

## `upright(jpeg)`

Turn a JPEG upright without decoding it to pixels: the EXIF orientation is applied
to the image itself and the tag set to 1, with no loss of quality, so
viewers that ignore EXIF show it right too. The entropy coding is
optimized as in `optimize`, so the result may be progressive; pass it to
`optimize(..., { baseline: true })` for baseline output. Returns
`undefined` where `optimize` does, and when the image is smaller than one
MCU on an axis that is mirrored.

An axis that is mirrored must be a whole number of MCUs (8 or 16 pixels
in most files); otherwise the last partial row or column (up to 15 pixels
in most files) is cut off. `info` on the result gives the final size.

## `stripMetadata(jpeg)`

Remove EXIF (including GPS position), XMP, comments and other metadata
without touching the image data. Keeps the color profile and orientation.
Returns `undefined` if the file cannot be parsed.

## `info(jpeg)`

Read the header without decoding: `{ width, height, components,
progressive, orientation }`, or `undefined` if the bytes are not a JPEG
this library understands. `orientation` is the EXIF orientation, 1 to 8
(1 when the file has none).

## `decode(jpeg)`

Decode a grayscale or color JPEG to 1-channel gray or 3-channel RGB
pixels, as stored. Returns `undefined` for CMYK files, codings this
library does not decode, malformed input, or frames over 100 megapixels.

## `applyOrientation(image, orientation)`

Turn pixels upright for an EXIF orientation (`info(jpeg).orientation`).
Orientations 5 to 8 swap width and height; a number outside 1 to 8 leaves
the pixels as they are.

## `resize(image, width, height, options?)`

Resize with a Lanczos3 filter. 4 channels are filtered independently.
The new width and height must be from 1 to 65535, as for a JPEG. Results
over 100 megapixels are refused, and so is a new width that, times the old
height, is over 100 megapixels (the filter's working image).

- `linear`: filter in linear light, which keeps fine light and dark
  detail from darkening when reducing. Assumes sRGB. With 4 channels it
  throws an `Error`.

## `crop(image, { x, y, width, height })`

Copy a rectangle out of an image. Returns `undefined` if the rectangle is
empty or not entirely inside the image.

## `fitWithin(width, height, maxWidth, maxHeight)`

The largest `[width, height]` that fits inside `maxWidth` x `maxHeight`
with the same aspect ratio. Never enlarges.

## Errors

- Functions that read JPEG bytes accept untrusted input and return
  `undefined` for anything they cannot handle.
- Arguments of the wrong kind throw a `TypeError`: JPEG bytes must be a
  `Uint8Array` (a Node `Buffer` is one), pixel data a `Uint8Array` or
  `Uint8ClampedArray`, options an object, flags booleans.
- Values out of range throw a `RangeError`: sizes, coordinates and
  `quality` must be integers in range, and `data` must have exactly
  `width * height * channels` bytes.
- Two limits of the format and filter throw a plain `Error`: `encode` with
  a width or height above 65535, and `resize` with `linear` on 4 channels.
- Messages start with `smallpeg:`.
- A `WebAssembly.RuntimeError` (a trap, such as running out of memory on
  a very large image) means the module may be left in an inconsistent
  state: in Node restart the process, and in the browser
  prefer the [worker](#in-a-web-worker), which stops itself and can be
  replaced.

## In a Web Worker

The functions are synchronous, and encoding or decoding a large photo
takes long enough to freeze a page for a moment. `createWorker` starts a
Web Worker that loads the wasm by itself, and returns the same functions
taking the same arguments and returning promises:

```js
import { createWorker } from "smallpeg/worker";

const smallpeg = createWorker();
const jpeg = await smallpeg.encode(ctx.getImageData(0, 0, w, h), { quality: 80 });
const blob = new Blob([jpeg], { type: "image/jpeg" });
smallpeg.terminate(); // when done
```

- Arguments are copied to the worker, so your buffers stay usable. A
  `Uint8Array` that views only part of a larger buffer, passed directly or
  as the `data` of a plain image object, is copied on its own, not with
  the whole buffer. Returned bytes
  and image `data` are moved back without a copy.
- Calls on one worker run one at a time, in order. Create several workers
  to run in parallel. Each call sends its arguments and result across, so
  a chain such as decode, resize and encode copies the pixels on each
  step.
- Errors reject the promise with the same class and message the function
  throws: `TypeError`, `RangeError` or `Error`. An argument that cannot be
  sent to a worker (such as a function) is a `TypeError`.
- `ready` settles once the wasm has loaded, and rejects if the worker
  could not start or load it; every call then rejects with that error.
  Calls wait for it themselves, so awaiting it is optional.
- `terminate()` stops the worker at once, which is also the way to cancel
  a call: calls still running or waiting, and any made later, reject with
  `Error("smallpeg: worker terminated")`. There is no progress reporting.
- `createWorker({ wasm })` loads the `.wasm` from elsewhere: a URL (string
  or `URL`; a relative one resolves against the page), an `ArrayBuffer` or
  `Uint8Array`, or a `WebAssembly.Module`. Unlike `init`, it cannot take a
  `Response` or a promise, which cannot be sent to a worker; anything else
  makes `ready` and every call reject with a `TypeError`.
- Bundlers that understand `new Worker(new URL(..., import.meta.url))`
  bundle the worker script. A development server that pre-bundles
  dependencies (Vite's, for one) can break that relative URL, as it can the
  `.wasm` one; leaving the package out of pre-bundling
  (`optimizeDeps.exclude` in Vite) avoids it.
- Loaded from another origin (a CDN), the package starts its worker
  through a same-origin script that imports it, which needs that origin
  to allow CORS.
- `createWorker` throws a `TypeError` where Web Workers are not available,
  such as in Node.
- A call that traps inside the wasm (running out of memory on a very
  large image, for example) rejects with a plain `Error` whose `name` is
  `"RuntimeError"`. The worker then stops, since the module may be left
  in an inconsistent state: later calls reject, and `terminate()` it and
  create a new one to go on.

## Experimental: JPEG XL

`smallpeg/jxl` recompresses a JPEG as JPEG XL without changing the image.
It is experimental: the function and its output may change in any
release.

```js
import { init, toJxl } from "smallpeg/jxl";

await init(); // in the browser; does nothing in Node
const out = toJxl(jpeg);
if (out) {
  // out.data is the JPEG XL file.
  if (out.reconstruction) {
    // A JPEG XL decoder can write the original JPEG back from it, byte
    // for byte, so the JPEG need not be kept.
  }
}
```

### `toJxl(jpeg, options?)`

Returns `{ data, reconstruction }`, or `undefined` for a file it cannot
convert (CMYK and RGB-coded JPEGs, frames over 50 megapixels, and input
it cannot read). Color and
grayscale JPEGs, baseline and progressive. The JPEG's coefficients are
stored as they are, so the image is the same; a photo's file comes out
about 10% smaller. The color profile, Exif and XMP carry over
(`stripMetadata` first removes Exif and XMP).

- `reconstruction` (option, default true): include what a decoder needs
  to rebuild the JPEG file itself. A few hundred bytes for most files,
  plus whatever the file holds beyond the image and that metadata, such
  as data after the end of the image.
- `reconstruction` (in the result): whether that data is there. False
  when the option is off, and when the format cannot describe the file
  (for example one that changes its restart interval between scans); the
  image is converted all the same.

It takes about 12 to 55 bytes of memory per pixel, more for full-color
(4:4:4), high-quality and noisy images: a 24-megapixel photo needs about
400 MB. The 50-megapixel limit keeps the worst case within what a
WebAssembly module can address. Should the module still trap (a
`WebAssembly.RuntimeError`), it may be left unusable: restart the
process in Node, reload the page in the browser.

### Loading

The entry point has a `.wasm` file of its own
(`jxl-web/smallpeg_jxl_wasm_bg.wasm`), about the size of the main one, so
pages that do not use it do not load it. `init` and `initSync` work as for
the main entry points, on that file: loading one does not load the other.
`"smallpeg/jxl"` resolves to the Node build in Node and to the browser
build elsewhere; `"smallpeg/jxl/web"` is the browser build explicitly.
There is no worker entry point for it yet.

## Example: shrink a photo for upload

```js
import { decode, info, applyOrientation, fitWithin, resize, encode } from "smallpeg";

function shrink(jpeg, maxSide = 1600) {
  const img = decode(jpeg);
  if (!img) return undefined;
  const upright = applyOrientation(img, info(jpeg).orientation);
  const [w, h] = fitWithin(upright.width, upright.height, maxSide, maxSide);
  return encode(resize(upright, w, h, { linear: true }), { quality: 80 });
}
```
