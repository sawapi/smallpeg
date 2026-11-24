// The message protocol between createWorker() (worker.mts) and the worker
// it starts (worker-main.mts). Kept apart from both so it can be tested in
// Node over a MessageChannel, which Web Workers are not available for.
//
// client -> worker: { type: "init", wasm }, then { type: "call", id, fn, args }
// worker -> client: { type: "ready" } or { type: "failed", error },
//                   then { type: "result", id, value } or { type: "error", id, error }

import type { Api } from "./core.mjs";
import type { JxlApi } from "./jxl-core.mjs";

/**
 * The part of a `Worker`, `MessagePort` or worker global scope used here,
 * written out so the declarations need no DOM types.
 */
export interface Port {
  postMessage(message: unknown, transfer: ArrayBuffer[]): void;
  onmessage: ((event: { data: unknown }) => void) | null;
  onmessageerror: ((event: unknown) => void) | null;
}

/** What every worker client has besides its functions. */
export interface WorkerControls {
  /**
   * Settles when the worker has loaded the wasm. Rejects if it could not
   * start or load it; every call then rejects with the same error.
   * Awaiting it is optional: calls wait for it themselves.
   */
  readonly ready: Promise<void>;
  /**
   * Stop the worker at once. Calls still running or waiting, and any made
   * later, reject with `Error("smallpeg: worker terminated")`. Calling it
   * again does nothing.
   */
  terminate(): void;
}

/**
 * The functions of `A` run in a worker: same arguments, the result as a
 * promise.
 */
export type InWorker<A> = {
  [K in keyof A]: A[K] extends (...args: infer P) => infer R ? (...args: P) => Promise<Awaited<R>> : never;
} & WorkerControls;

/** {@link Api} with every function run in a worker. */
export type WorkerApi = InWorker<Api>;

// Every function of Api, for the client to forward. The check below fails
// to compile if Api gains or loses one.
export const names = [
  "encode",
  "optimize",
  "upright",
  "stripMetadata",
  "info",
  "decode",
  "applyOrientation",
  "resize",
  "crop",
  "fitWithin",
] as const;
type Assert<T extends true> = T;
type _Exhaustive = Assert<
  [keyof Api] extends [(typeof names)[number]]
    ? [(typeof names)[number]] extends [keyof Api]
      ? true
      : false
    : false
>;

// The same for the functions of smallpeg/jxl, which a worker of their own
// serves. Here, not beside them, so that worker's client loads no more
// than this file.
export const jxlNames = ["toJxl"] as const;
type _JxlExhaustive = Assert<
  [keyof JxlApi] extends [(typeof jxlNames)[number]]
    ? [(typeof jxlNames)[number]] extends [keyof JxlApi]
      ? true
      : false
    : false
>;

interface WireError {
  name: string;
  message: string;
}
type ClientMessage =
  | { type: "init"; wasm: unknown }
  | { type: "call"; id: number; fn: string; args: unknown[] };
type WorkerMessage =
  | { type: "ready" }
  | { type: "failed"; error: WireError }
  | { type: "result"; id: number; value: unknown }
  | { type: "error"; id: number; error: WireError };

// Errors are sent as plain data and rebuilt on the other side, which is
// the same in every browser (structured cloning of errors is not).
function toWire(e: unknown): WireError {
  try {
    if (e instanceof Error) return { name: String(e.name), message: String(e.message) };
    return { name: "Error", message: String(e) };
  } catch {
    return { name: "Error", message: "smallpeg: unknown error" }; // String() itself threw
  }
}

function fromWire(w: WireError): Error {
  switch (w.name) {
    case "TypeError":
      return new TypeError(w.message);
    case "RangeError":
      return new RangeError(w.message);
    default: {
      const e = new Error(w.message);
      // Keep the kind (e.g. a wasm RuntimeError) for the message, not the class.
      if (w.name !== "Error") e.name = w.name;
      return e;
    }
  }
}

type View = Uint8Array | Uint8ClampedArray;

// A byte array that owns its whole (non-shared) buffer, which can be moved.
function whole(v: unknown): v is View & { buffer: ArrayBuffer } {
  return (
    (v instanceof Uint8Array || v instanceof Uint8ClampedArray) &&
    v.buffer instanceof ArrayBuffer &&
    v.byteOffset === 0 &&
    v.byteLength === v.buffer.byteLength
  );
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  if (typeof v !== "object" || v === null) return false;
  const proto = Object.getPrototypeOf(v);
  return proto === Object.prototype || proto === null;
}

/**
 * The buffers a result can move instead of copy: the result itself when it
 * is a byte array (encode, optimize), or byte arrays directly inside it
 * (the `data` of an image), each owning its whole buffer.
 */
export function transferables(value: unknown): ArrayBuffer[] {
  const out: ArrayBuffer[] = [];
  const add = (v: unknown) => {
    if (whole(v) && !out.includes(v.buffer)) out.push(v.buffer);
  };
  add(value);
  if (isPlainObject(value)) for (const v of Object.values(value)) add(v);
  return out;
}

/**
 * Prepare an argument for sending. Structured cloning copies a view's whole
 * underlying buffer, so a byte array viewing only part of one (a
 * `subarray`) is copied on its own, at the top level or directly inside a
 * plain object (an image's `data`); other objects are cloned as they are. The copies are fresh, so they are moved
 * rather than copied a second time: `transfer` collects their buffers. The
 * caller's arrays are never moved.
 */
export function compact(v: unknown, transfer: ArrayBuffer[]): unknown {
  const one = (x: unknown): unknown => {
    if ((x instanceof Uint8Array || x instanceof Uint8ClampedArray) && !whole(x)) {
      // The constructor, not slice(): a Node Buffer's slice() is a view of
      // the same memory, and moving that would take the caller's buffer.
      const copy = x instanceof Uint8ClampedArray ? new Uint8ClampedArray(x) : new Uint8Array(x);
      if (copy.buffer !== x.buffer) transfer.push(copy.buffer);
      return copy;
    }
    return x;
  };
  if (isPlainObject(v)) {
    // Each property read once, so a getter cannot change between reads.
    const entries = Object.entries(v);
    const mapped = entries.map(([k, x]) => [k, one(x)] as const);
    return mapped.some(([, y], i) => y !== entries[i]![1]) ? Object.fromEntries(mapped) : v;
  }
  return one(v);
}

/**
 * Worker side: wait for the init message, load the wasm with `init`, then
 * answer calls one at a time, in the order they arrive.
 */
export function serve(port: Port, api: Api, init: (wasm: unknown) => Promise<void>): void {
  serveFns(port, api, init, names);
}

/** {@link serve} for any set of functions: `api` answers those named in `fns`. */
export function serveFns<A>(
  port: Port,
  api: A,
  init: (wasm: unknown) => Promise<void>,
  fns: readonly (keyof A & string)[],
): void {
  let ready: Promise<void> | undefined;
  let queue = Promise.resolve();
  let stopped: WireError | undefined;
  port.onmessage = ({ data }) => {
    const msg = data as ClientMessage;
    if (msg.type === "init") {
      if (ready) return;
      // async, so that init throwing synchronously also becomes a rejection.
      ready = (async () => init(msg.wasm))();
      ready
        .then(
          () => port.postMessage({ type: "ready" } satisfies WorkerMessage, []),
          (e: unknown) => port.postMessage({ type: "failed", error: toWire(e) } satisfies WorkerMessage, []),
        )
        .catch(() => {}); // nothing left to tell the client with
      return;
    }
    if (msg.type !== "call") return;
    const { id, fn, args } = msg;
    const job = async () => {
      try {
        if (stopped) throw fromWire(stopped);
        if (!ready) throw new Error("smallpeg: the worker was not initialized");
        await ready;
        if (!(fns as readonly string[]).includes(fn)) throw new TypeError(`smallpeg: no function ${fn}`);
        const value: unknown = (api[fn as keyof A] as (...a: unknown[]) => unknown)(...args);
        port.postMessage({ type: "result", id, value } satisfies WorkerMessage, transferables(value));
      } catch (e) {
        port.postMessage({ type: "error", id, error: toWire(e) } satisfies WorkerMessage, []);
        // A trap (a Rust panic, or running out of memory) can leave the
        // module's state inconsistent: stop serving rather than reuse it.
        if (!stopped && typeof WebAssembly !== "undefined" && e instanceof WebAssembly.RuntimeError) {
          stopped = {
            name: "Error",
            message: `smallpeg: the worker stopped after a WebAssembly error (${toWire(e).message}); create a new one`,
          };
          port.postMessage({ type: "failed", error: stopped } satisfies WorkerMessage, []);
        }
      }
    };
    // Whatever happens to one call, the next still runs.
    queue = queue.then(job).catch(() => {});
  };
  // A message that cannot be read has no id to answer: fail everything.
  port.onmessageerror = () => {
    const error = { name: "Error", message: "smallpeg: a message to the worker could not be read" };
    port.postMessage({ type: "failed", error } satisfies WorkerMessage, []);
  };
}

/**
 * Client side: send the init message, and return the API that forwards
 * calls over `port`. `stop` ends the worker; `fail` rejects everything
 * with an error found outside the protocol (such as the worker's `error`
 * event).
 */
export function connect(
  port: Port,
  wasm: unknown,
  stop: () => void,
): { api: WorkerApi; fail: (e: Error) => void } {
  return connectFns<Api>(port, wasm, stop, names);
}

/** {@link connect} for any set of functions: those named in `fns`. */
export function connectFns<A>(
  port: Port,
  wasm: unknown,
  stop: () => void,
  fns: readonly (keyof A & string)[],
): { api: InWorker<A>; fail: (e: Error) => void } {
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  let nextId = 0;
  let dead: Error | undefined;
  let settle!: { resolve: () => void; reject: (e: Error) => void };
  const ready = new Promise<void>((resolve, reject) => {
    settle = { resolve, reject };
  });
  // Calls report the failure too: not awaiting `ready` is not a mistake.
  ready.catch(() => {});

  function fail(e: Error): void {
    if (dead) return;
    dead = e;
    settle.reject(e);
    for (const p of pending.values()) p.reject(e);
    pending.clear();
  }

  // postMessage throws DataCloneError (a DOMException) for a function or
  // another value that cannot be copied to the worker.
  function sendError(e: unknown): Error {
    if (typeof e === "object" && e !== null && (e as { name?: unknown }).name === "DataCloneError") {
      return new TypeError("smallpeg: arguments must be structured-cloneable");
    }
    return e instanceof Error ? e : new Error(String(e));
  }

  port.onmessage = ({ data }) => {
    const msg = data as WorkerMessage;
    switch (msg.type) {
      case "ready":
        settle.resolve();
        break;
      case "failed":
        fail(fromWire(msg.error));
        break;
      case "result":
      case "error": {
        const p = pending.get(msg.id);
        if (!p) return;
        pending.delete(msg.id);
        if (msg.type === "result") p.resolve(msg.value);
        else p.reject(fromWire(msg.error));
        break;
      }
    }
  };
  port.onmessageerror = () => fail(new Error("smallpeg: a message from the worker could not be read"));

  function call(fn: string, args: unknown[]): Promise<unknown> {
    if (dead) return Promise.reject(dead);
    return new Promise((resolve, reject) => {
      const id = ++nextId;
      try {
        const transfer: ArrayBuffer[] = [];
        const sent = args.map((a) => compact(a, transfer));
        port.postMessage({ type: "call", id, fn, args: sent } satisfies ClientMessage, transfer);
      } catch (e) {
        reject(sendError(e));
        return;
      }
      pending.set(id, { resolve, reject });
    });
  }

  let terminated = false;
  const api = {
    ready,
    terminate(): void {
      if (terminated) return;
      terminated = true;
      const e = new Error("smallpeg: worker terminated");
      fail(e);
      dead = e; // also after an earlier failure: later calls say why
      stop();
    },
  } as Record<string, unknown>;
  for (const fn of fns) api[fn] = (...args: unknown[]) => call(fn, args);

  try {
    const transfer: ArrayBuffer[] = [];
    port.postMessage({ type: "init", wasm: compact(wasm, transfer) } satisfies ClientMessage, transfer);
  } catch (e) {
    fail(sendError(e));
  }
  return { api: api as InWorker<A>, fail };
}
