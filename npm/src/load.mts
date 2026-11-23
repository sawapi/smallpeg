// Loading a wasm-bindgen module in the browser: shared by the browser
// entry points, each of which has a module of its own.

/** What the glue's default export (the async loader) accepts. */
type InitWasm = (arg?: never) => Promise<unknown>;
/** What the glue's `initSync` accepts. */
type InitSyncWasm = (arg: never) => unknown;

export interface Loader {
  init(wasm?: string | object): Promise<void>;
  initSync(wasm: object): void;
  /** Throws unless the module has loaded. */
  ensureReady(): void;
}

// A plain object carrying `key` is wasm-bindgen's own argument form, which
// 0.13 documented: pass it through. Anything else is the source itself.
function isPlain(x: unknown, key: string): boolean {
  return (
    typeof x === "object" && x !== null && Object.getPrototypeOf(x) === Object.prototype && key in x
  );
}

/** `notReady` is the message for a call made before the module loaded. */
export function makeLoader(initWasm: InitWasm, initSyncWasm: InitSyncWasm, notReady: string): Loader {
  let ready: Promise<void> | undefined;
  let loaded = false;

  function init(wasm?: string | object): Promise<void> {
    // A plain object must be wasm-bindgen's form; `{}` or initSync's
    // `{ module }` here is a mistake, not something to fetch around.
    if (
      typeof wasm === "object" &&
      wasm !== null &&
      Object.getPrototypeOf(wasm) === Object.prototype &&
      !("module_or_path" in wasm)
    ) {
      return Promise.reject(
        new TypeError("smallpeg: init() takes a URL, Response, bytes or WebAssembly.Module"),
      );
    }
    // The casts: the source type is deliberately looser than the glue's DOM types.
    const arg =
      wasm === undefined
        ? undefined
        : isPlain(wasm, "module_or_path")
          ? (wasm as never)
          : ({ module_or_path: wasm } as never);
    ready ??= initWasm(arg).then(
      () => {
        loaded = true;
      },
      (err: unknown) => {
        ready = undefined;
        throw err;
      },
    );
    return ready;
  }

  function initSync(wasm: object): void {
    if (loaded) return;
    // The glue would instantiate a second copy under the pending load.
    if (ready) throw new Error("smallpeg: init() is still loading; await it instead");
    initSyncWasm(isPlain(wasm, "module") ? (wasm as never) : ({ module: wasm } as never));
    loaded = true;
    ready = Promise.resolve();
  }

  function ensureReady(): void {
    if (!loaded) throw new Error(notReady);
  }

  return { init, initSync, ensureReady };
}
