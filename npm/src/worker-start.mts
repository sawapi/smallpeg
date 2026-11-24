// What the worker entry points share: checking the options, starting the
// worker script, and wiring it to the protocol. Not an entry point.

import { connectFns, type InWorker, type Port } from "./protocol.mjs";

/** Options for `createWorker`. */
export interface WorkerOptions {
  /**
   * Where the worker loads the `.wasm` from: a URL (string or `URL`,
   * relative ones resolved against the page), the bytes (`ArrayBuffer` or
   * `Uint8Array`, copied), or a compiled `WebAssembly.Module`. Unlike
   * `init`, a `Response` or a promise cannot be sent to a worker. By
   * default, the file inside the package. Typed loosely so the
   * declarations need no DOM types.
   */
  wasm?: string | object | undefined;
}

// What can be sent to the worker for init(), with URLs made absolute (a
// relative one would resolve against the worker's script, and a URL
// object cannot be cloned). undefined when `wasm` is none of those.
function wasmSource(wasm: unknown): { value: unknown } | undefined {
  if (wasm === undefined) return { value: undefined };
  if (typeof wasm === "string" || (typeof URL !== "undefined" && wasm instanceof URL)) {
    // As the page resolves it (a <base> element included); in a worker,
    // against the worker's own location.
    const g = globalThis as { document?: { baseURI: string }; location?: { href: string } };
    const base = g.document?.baseURI ?? g.location?.href;
    try {
      return { value: new URL(wasm, base).href };
    } catch {
      return undefined; // not a URL
    }
  }
  if (
    wasm instanceof ArrayBuffer ||
    wasm instanceof Uint8Array ||
    (typeof WebAssembly !== "undefined" && wasm instanceof WebAssembly.Module)
  ) {
    return { value: wasm };
  }
  return undefined;
}

// The part of a `Worker` used here: the declarations need no DOM types.
interface Started extends Port {
  terminate(): void;
  onerror: ((event: { message?: string; preventDefault(): void }) => void) | null;
}

/**
 * Start a worker and return its client for the functions named in `fns`.
 * `spawn` starts the script from this package's origin (each entry point
 * writes its `new Worker(new URL(...))` out, for bundlers to see);
 * `script` is that script's name beside this file, for the other way
 * below.
 */
export function startWorker<A>(
  options: WorkerOptions | undefined,
  fns: readonly (keyof A & string)[],
  spawn: () => object,
  script: string,
): InWorker<A> {
  // A client with no worker, whose `ready` and every call reject with `e`.
  const failed = (e: Error): InWorker<A> => {
    const none: Port = { postMessage() {}, onmessage: null, onmessageerror: null };
    const c = connectFns<A>(none, undefined, () => {}, fns);
    c.fail(e);
    return c.api;
  };
  if (typeof Worker === "undefined") throw new TypeError("smallpeg: createWorker needs Web Workers");
  if (options !== undefined && (typeof options !== "object" || options === null || Array.isArray(options))) {
    return failed(new TypeError("smallpeg: options must be an object"));
  }
  let source: { value: unknown } | undefined;
  try {
    source = wasmSource(options?.wasm);
  } catch (e) {
    // A getter or proxy that throws: a rejection like any other failure.
    return failed(e instanceof Error ? e : new TypeError(String(e)));
  }
  if (!source) {
    return failed(
      new TypeError("smallpeg: wasm must be a URL, an ArrayBuffer, a Uint8Array or a WebAssembly.Module"),
    );
  }
  let worker: Started;
  try {
    worker = spawn() as Started;
  } catch (e) {
    // A worker script must come from the page's origin. Loaded from
    // another one (a CDN), start a same-origin worker that imports it;
    // module imports may cross origins (with CORS).
    try {
      // Not a literal in `new URL`: bundlers would copy the file unbundled.
      const href = new URL(script, import.meta.url).href;
      const blob = new Blob([`import ${JSON.stringify(href)};`], { type: "text/javascript" });
      const url = URL.createObjectURL(blob);
      try {
        worker = new Worker(url, { type: "module" }) as unknown as Started;
      } finally {
        // The worker has its own reference to the script once created.
        setTimeout(() => URL.revokeObjectURL(url), 60_000);
      }
    } catch {
      return failed(
        new Error("smallpeg: the worker failed to start" + (e instanceof Error ? `: ${e.message}` : "")),
      );
    }
  }
  const c = connectFns<A>(worker, source.value, () => worker.terminate(), fns);
  worker.onerror = (e) => {
    e.preventDefault();
    c.fail(new Error("smallpeg: the worker failed" + (e.message ? `: ${e.message}` : " to start")));
  };
  return c.api;
}
