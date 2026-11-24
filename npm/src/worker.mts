// Worker entry point (browsers and bundlers): the same functions as the
// browser build, run in a Web Worker so the page stays responsive.

import { type Api } from "./core.mjs";
import { names, type WorkerApi } from "./protocol.mjs";
import { startWorker, type WorkerOptions } from "./worker-start.mjs";

export type {
  Bytes,
  Channels,
  EncodeOptions,
  Image,
  ImageInput,
  JpegInfo,
  OptimizeOptions,
  Orientation,
  Rect,
  ResizeOptions,
} from "./core.mjs";
export type { WorkerApi } from "./protocol.mjs";

export type { WorkerOptions } from "./worker-start.mjs";

/**
 * Start a Web Worker that runs the functions of the browser build, and
 * return them as functions that take the same arguments and return
 * promises. The worker loads the wasm by itself; there is no `init` to
 * call.
 *
 * Arguments are copied to the worker (your buffers stay usable); returned
 * bytes and image data are moved back without a copy. Calls on one worker
 * run one at a time, in order: create several workers to run in parallel.
 * Errors reject the promise with the same kind of error the function
 * throws.
 *
 * @throws {TypeError} where Web Workers are not available (such as Node).
 */
export function createWorker(options?: WorkerOptions): WorkerApi {
  return startWorker<Api>(
    options,
    names,
    // Written out in full so bundlers see the worker and bundle it.
    () => new Worker(new URL("./worker-main.mjs", import.meta.url), { type: "module" }),
    "./worker-main.mjs",
  );
}
