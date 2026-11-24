# smallpeg

[![npm](https://img.shields.io/npm/v/smallpeg.svg)](https://www.npmjs.com/package/smallpeg)
[![CI](https://github.com/sawapi/smallpeg/actions/workflows/ci.yml/badge.svg)](https://github.com/sawapi/smallpeg/actions/workflows/ci.yml)
[![license](https://img.shields.io/npm/l/smallpeg.svg)](#license)

A JPEG encoder, lossless optimizer and decoder for Node and the browser,
compiled to WebAssembly from a dependency-free Rust library, with
TypeScript types.

[Website](https://sawapi.github.io/smallpeg/) ·
[API](https://github.com/sawapi/smallpeg/blob/main/docs/npm-api.md) ·
[GitHub](https://github.com/sawapi/smallpeg) ·
[Changelog](https://github.com/sawapi/smallpeg/blob/main/CHANGELOG.md)

## Install

```sh
npm install smallpeg
```

## Node

```js
import { encode, optimize } from "smallpeg";
import { readFileSync, writeFileSync } from "node:fs";

// data: width * height * channels bytes (1 gray, 3 RGB, 4 RGBA)
const jpeg = encode({ data: rgb, width, height }, { quality: 80 });

// Same pixels, fewer bytes (an already optimized file may not shrink).
const input = readFileSync("photo.jpg");
const smaller = optimize(input);
if (smaller && smaller.length < input.length) writeFileSync("photo.jpg", smaller);
```

## Browser

```js
import { init, encode } from "smallpeg/web";

await init(); // loads the .wasm file once
const jpeg = encode(ctx.getImageData(0, 0, w, h)); // RGBA, alpha onto white
const blob = new Blob([jpeg], { type: "image/jpeg" });
```

Encoding is synchronous. To keep the page responsive, run it in a Web
Worker that the package starts for you:

```js
import { createWorker } from "smallpeg/worker";

const smallpeg = createWorker(); // loads the wasm by itself
const jpeg = await smallpeg.encode(ctx.getImageData(0, 0, w, h));
```

## Features

- **Encode** gray, RGB or RGBA pixels, such as a canvas `ImageData`.
- **Optimize** an existing JPEG losslessly: same pixels, fewer bytes.
- **Turn upright** a phone photo by its EXIF orientation, losslessly.
- **Strip metadata** such as EXIF and GPS position, keeping the color
  profile and orientation.
- **Decode**, read the header, fix EXIF orientation, crop and resize.
- **Handle untrusted input**: functions that read JPEG bytes return
  `undefined` for anything they cannot handle.

| Function | Returns |
|---|---|
| `encode(image, { quality, fullColor })` | JPEG bytes |
| `optimize(jpeg, { baseline })` | Re-coded JPEG with identical pixels, or `undefined` |
| `upright(jpeg)` | JPEG turned upright losslessly, or `undefined` |
| `stripMetadata(jpeg)` | JPEG without metadata, or `undefined` |
| `info(jpeg)` | `{ width, height, components, progressive, orientation }`, or `undefined` |
| `decode(jpeg)` | Image (gray or RGB), or `undefined` |
| `applyOrientation(image, orientation)` | Upright image |
| `resize(image, width, height, { linear })` | Resized image |
| `crop(image, { x, y, width, height })` | Cropped image, or `undefined` |
| `fitWithin(width, height, maxWidth, maxHeight)` | `[width, height]` that fits |
| `init(wasm?)`, `initSync(bytes)` | Load the WebAssembly module (browser build; no-ops in Node) |
| `createWorker({ wasm? })` (from `/worker`) | The functions above returning promises, run in a Web Worker; `ready`, `terminate()` |
| `toJxl(jpeg, { reconstruction })` (from `/jxl`, experimental) | `{ data, reconstruction }`: the JPEG as JPEG XL with the same image, or `undefined` |
| `createWorker({ wasm? })` (from `/jxl/worker`, experimental) | `toJxl` returning a promise, run in a Web Worker; `ready`, `terminate()` |

Images are plain `{ data, width, height, channels }` objects, and every
image a function returns can be passed to the next. Details, options and
errors are in the [API documentation](https://github.com/sawapi/smallpeg/blob/main/docs/npm-api.md).

## Requirements

Node 22.12 or later, or a current browser (directly or through a bundler;
Safari 16.4 or later).
TypeScript types need TypeScript 5.7 or later.

## License

MIT OR Apache-2.0, at your option. The Rust library is
[`smallpeg` on crates.io](https://crates.io/crates/smallpeg).
