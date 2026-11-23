// Node entry point for `smallpeg/jxl`: the wasm is loaded synchronously
// when this module is imported. `init`, `initSync` and the default export
// mirror the browser build; their arguments are ignored.

import * as glue from "../jxl-node/smallpeg_jxl_wasm.js";
import { type JxlApi, type JxlParamsMatch, makeJxlApi } from "./jxl-core.mjs";

// Fails to compile if the glue's parameters drift from what jxl-core expects.
type Assert<T extends { [K in keyof T]: true }> = T;
type _Check = Assert<JxlParamsMatch<typeof glue>>;

const api = makeJxlApi(glue);

/** Does nothing in Node; present so code shared with the browser build can always `await init()`. */
export async function init(_wasm?: unknown): Promise<void> {}

/** Does nothing in Node; present so code shared with the browser build can call `initSync(bytes)`. */
export function initSync(_module?: unknown): void {}

export default init;

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
