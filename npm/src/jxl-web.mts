// Browser (and bundler) entry point for `smallpeg/jxl`: its wasm has to
// be fetched and instantiated with `await init()` (or `initSync()`) before
// the first call. It is a module of its own, separate from the main one.

import initWasm, * as glue from "../jxl-web/smallpeg_jxl_wasm.js";
import { type JxlApi, type JxlParamsMatch, makeJxlApi } from "./jxl-core.mjs";
import { makeLoader } from "./load.mjs";

// Fails to compile if the glue's parameters drift from what jxl-core expects.
type Assert<T extends { [K in keyof T]: true }> = T;
type _Check = Assert<JxlParamsMatch<typeof glue>>;

/**
 * Where to load the `.wasm` from: a URL (string or `URL`), a `Request`, a
 * `Response` or a promise of one (e.g. `fetch(url)`), the bytes, or a
 * compiled `WebAssembly.Module`. Typed loosely so the declarations need no
 * DOM types.
 */
export type WasmSource = string | object;

/** What {@link initSync} takes: the bytes or a compiled `WebAssembly.Module`. */
export type WasmModuleSource = object;

// Says whose init, since the main entry points have another.
const loader = makeLoader(
  initWasm as never,
  glue.initSync as never,
  "smallpeg: call `await init()` from smallpeg/jxl before using it in the browser",
);

/**
 * Load the wasm. Call (and await) it once before `toJxl`; later calls
 * return the same promise, and a failed load can be retried.
 *
 * By default the file is fetched from `jxl-web/smallpeg_jxl_wasm_bg.wasm`
 * inside the package (bundlers that understand
 * `new URL(..., import.meta.url)` copy it). Pass `wasm` to load it from
 * elsewhere. It is not the main entry points' file: each has its own.
 */
export function init(wasm?: WasmSource): Promise<void> {
  return loader.init(wasm);
}

/**
 * Load the wasm synchronously from bytes you already have (or a compiled
 * `WebAssembly.Module`). Browsers may refuse to compile a module
 * synchronously on the main thread; use it in workers, or prefer
 * {@link init}.
 */
export function initSync(wasm: WasmModuleSource): void {
  loader.initSync(wasm);
}

export default init;

const api = makeJxlApi(glue, loader.ensureReady);

export type { Bytes, JxlOptions, JxlOutput } from "./jxl-core.mjs";

/**
 * Recompress a JPEG as JPEG XL without changing its coefficients: the
 * image is the same, and a photo's file about 10% smaller. Color and
 * grayscale JPEGs; `undefined` for CMYK and RGB-coded files, frames over
 * 100 megapixels, and input this cannot read. The color profile, Exif and
 * XMP carry over.
 *
 * `reconstruction` in the result says whether a JPEG XL decoder can write
 * the original JPEG file back from it byte for byte.
 *
 * Experimental: this function and its output may change in any release.
 *
 * @throws {TypeError} for arguments of the wrong kind.
 */
export const toJxl: JxlApi["toJxl"] = api.toJxl;
