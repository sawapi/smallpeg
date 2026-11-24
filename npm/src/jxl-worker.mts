// Worker entry point for `smallpeg/jxl` (browsers and bundlers): `toJxl`
// run in a Web Worker, which loads that entry point's wasm, not the main
// one. Experimental like the function.

import type { JxlApi } from "./jxl-core.mjs";
import { type InWorker, jxlNames } from "./protocol.mjs";
import { startWorker, type WorkerOptions } from "./worker-start.mjs";

export type { Bytes, JxlOptions, JxlOutput } from "./jxl-core.mjs";
export type { WorkerOptions } from "./worker-start.mjs";

/** `toJxl` run in a worker, with the worker's `ready` and `terminate`. */
export type JxlWorkerApi = InWorker<JxlApi>;

/**
 * Start a Web Worker that runs `toJxl`, and return it as a function that
 * takes the same arguments and returns a promise. The worker loads the
 * wasm of `smallpeg/jxl` by itself; there is no `init` to call. A
 * conversion takes long enough, and enough memory, that a page does well
 * to keep it off its own thread; a worker that fails can also be replaced,
 * where a page would have to be reloaded.
 *
 * The JPEG is copied to the worker (your buffer stays usable); the
 * result's bytes are moved back without a copy. Calls on one worker run
 * one at a time, in order. Errors reject the promise with the same kind
 * of error the function throws. `wasm` in the options is the file of
 * `smallpeg/jxl` (`jxl-web/smallpeg_jxl_wasm_bg.wasm`), not the main one.
 *
 * Experimental: this may change in any release.
 *
 * @throws {TypeError} where Web Workers are not available (such as Node).
 */
export function createWorker(options?: WorkerOptions): JxlWorkerApi {
  return startWorker<JxlApi>(
    options,
    jxlNames,
    // Written out in full so bundlers see the worker and bundle it.
    () => new Worker(new URL("./jxl-worker-main.mjs", import.meta.url), { type: "module" }),
    "./jxl-worker-main.mjs",
  );
}
