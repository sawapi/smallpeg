// The worker protocol, run over a MessageChannel in Node (which has no Web
// Workers): `serve` on one end with the browser build, `connect` on the
// other, as createWorker() wires them. Run from test.mjs, after the
// browser build has been loaded and checked.
import assert from "node:assert/strict";
import { MessageChannel } from "node:worker_threads";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { connect, connectFns, jxlNames, names, serve, serveFns } from "./dist/protocol.mjs";

function message(f) {
  try {
    f();
  } catch (e) {
    return `${e.name}: ${e.message}`;
  }
}

export async function testWorker(web, jpeg) {
  // A pair of ports, the serving end calling `init` and `api` as given.
  function pair(api = web, init = (w) => web.init(w)) {
    const { port1, port2 } = new MessageChannel();
    serve(port2, api, init);
    return connect(port1, undefined, () => {
      port1.close();
      port2.close();
    });
  }

  const rgb = { data: new Uint8Array(32 * 24 * 3).map((_, i) => i * 7), width: 32, height: 24 };

  // Same results as calling the browser build directly; inputs are copied,
  // not taken.
  {
    const w = pair();
    await w.api.ready;
    const same = async (fn, ...args) =>
      assert.deepEqual(await w.api[fn](...args), web[fn](...args), `${fn} through the worker`);
    await same("encode", rgb, { quality: 90 });
    await same("optimize", jpeg, { baseline: true });
    await same("upright", jpeg);
    await same("stripMetadata", jpeg);
    await same("info", jpeg);
    await same("decode", jpeg);
    await same("decode", new Uint8Array([1, 2, 3])); // undefined comes back too
    await same("applyOrientation", rgb, 6);
    await same("resize", rgb, 16, 12, { linear: true });
    await same("crop", rgb, { x: 1, y: 2, width: 3, height: 4 });
    await same("fitWithin", 4000, 3000, 1000, 1000);
    assert.equal(rgb.data.byteLength, 32 * 24 * 3, "the caller's buffer is not detached");
    assert.ok(jpeg.byteLength > 0);

    // Errors keep their class and message.
    for (const [fn, ...args] of [
      ["encode", rgb, { quality: 0 }],
      ["info", "abc"],
      ["encode", { data: new Uint8Array(3 * 70000), width: 70000, height: 1 }],
    ]) {
      await assert.rejects(w.api[fn](...args), (e) => {
        assert.equal(`${e.name}: ${e.message}`, message(() => web[fn](...args)));
        assert.equal(Object.getPrototypeOf(e).constructor.name, e.name);
        return true;
      });
    }
    // A function cannot be sent: a rejection, not a throw.
    const p = w.api.encode(rgb, { quality: () => 80 });
    assert.ok(p instanceof Promise);
    await assert.rejects(p, /^TypeError: smallpeg: arguments must be structured-cloneable$/);

    // One call at a time, answered in order.
    const order = [];
    await Promise.all([
      w.api.encode(rgb).then(() => order.push("encode")),
      w.api.info(jpeg).then(() => order.push("info")),
      w.api.encode(rgb, { quality: 0 }).catch(() => order.push("error")),
      w.api.fitWithin(2, 2, 1, 1).then(() => order.push("fitWithin")),
    ]);
    assert.deepEqual(order, ["encode", "info", "error", "fitWithin"]);

    // terminate: pending and later calls reject; twice is fine.
    const running = w.api.decode(jpeg);
    w.api.terminate();
    w.api.terminate();
    await assert.rejects(running, /^Error: smallpeg: worker terminated$/);
    await assert.rejects(w.api.info(jpeg), /^Error: smallpeg: worker terminated$/);
  }

  // Returned bytes and image data are moved out of the worker, not copied.
  {
    const served = [];
    const keep = (fn) => (...a) => {
      const r = web[fn](...a);
      served.push(r);
      return r;
    };
    const w = pair({ ...web, encode: keep("encode"), decode: keep("decode") });
    const out = await w.api.encode(rgb);
    const img = await w.api.decode(out);
    assert.equal(served[0].byteLength, 0, "encode output transferred");
    assert.equal(served[1].data.byteLength, 0, "image data transferred");
    assert.ok(out.byteLength > 0 && img.data.byteLength === 32 * 24 * 3);
    w.api.terminate();
  }

  // A failed load rejects `ready`, calls already waiting, and later calls.
  {
    const w = pair(web, async () => {
      throw new WebAssembly.CompileError("bad wasm");
    });
    const waiting = w.api.info(jpeg);
    await assert.rejects(w.api.ready, (e) => e.name === "CompileError" && e.message === "bad wasm");
    await assert.rejects(waiting, /bad wasm/);
    await assert.rejects(w.api.encode(rgb), /bad wasm/);
    w.api.terminate();
    await assert.rejects(w.api.encode(rgb), /worker terminated/);
  }

  // An error from outside the protocol (the worker's error event).
  {
    const w = pair(web, () => new Promise(() => {})); // never loads
    const waiting = w.api.encode(rgb);
    w.fail(new Error("smallpeg: the worker failed to start"));
    w.fail(new Error("second")); // ignored
    await assert.rejects(w.api.ready, /failed to start/);
    await assert.rejects(waiting, /failed to start/);
    w.api.terminate();
  }

  // A thrown value that cannot even be turned into a string still gets an
  // answer, and the calls after it still run.
  {
    const evil = {
      toString() {
        throw new Error("no");
      },
    };
    const w = pair({
      ...web,
      encode: () => {
        throw evil;
      },
    });
    await assert.rejects(w.api.encode(rgb), /^Error: smallpeg: unknown error$/);
    assert.deepEqual(await w.api.fitWithin(4, 2, 2, 2), [2, 1]);
    w.api.terminate();
  }

  // After a WebAssembly trap the worker stops: that call rejects with the
  // trap, and every later call with the reason.
  {
    let calls = 0;
    let received = 0;
    const { port1, port2 } = new MessageChannel();
    serve(
      port2,
      {
        ...web,
        encode: () => {
          calls++;
          throw new WebAssembly.RuntimeError("unreachable");
        },
      },
      (x) => web.init(x),
    );
    const serveOne = port2.onmessage;
    port2.onmessage = (ev) => {
      received++;
      serveOne(ev);
    };
    const w = connect(port1, undefined, () => {
      port1.close();
      port2.close();
    });
    const stoppedMsg = /^Error: smallpeg: the worker stopped after a WebAssembly error \(unreachable\); create a new one$/;
    // The second call is already waiting when the first traps.
    const first = w.api.encode(rgb);
    const second = w.api.fitWithin(4, 2, 2, 2);
    await assert.rejects(first, (e) => e.name === "RuntimeError" && e.message === "unreachable");
    await assert.rejects(second, stoppedMsg);
    // Once the client has heard, later calls are refused without being sent.
    const sent = received;
    await assert.rejects(w.api.encode(rgb), stoppedMsg);
    await new Promise((r) => setTimeout(r, 20));
    assert.equal(received, sent);
    await w.api.ready; // it had loaded; the trap does not change that
    assert.equal(calls, 1);
    w.api.terminate();
  }

  // A message the worker cannot read fails everything, not just one call.
  {
    const sent = [];
    const fake = { postMessage: (m) => sent.push(m), onmessage: null, onmessageerror: null };
    serve(fake, web, (w) => web.init(w));
    fake.onmessageerror({});
    assert.deepEqual(sent, [
      { type: "failed", error: { name: "Error", message: "smallpeg: a message to the worker could not be read" } },
    ]);
  }

  // A view into part of a buffer is sent as its own copy, at the top level
  // or as an image's data, and that copy is moved; whole buffers are sent
  // as they are.
  {
    const sent = [];
    const fake = { postMessage: (m, t) => sent.push([m, t]), onmessage: null, onmessageerror: null };
    const c = connect(fake, undefined, () => {});
    const big = new Uint8Array(1 << 20);
    const image = { data: new Uint8ClampedArray(big.buffer, 12, 12), width: 2, height: 2 };
    const calls = [c.api.info(big.subarray(10, 20)), c.api.info(big), c.api.encode(image, { quality: 50 })];
    const [m1, t1] = sent[1];
    assert.equal(m1.args[0].buffer.byteLength, 10, "a subarray is compacted");
    assert.deepEqual(t1, [m1.args[0].buffer], "and its copy moved");
    assert.equal(sent[2][0].args[0], big, "a whole buffer is sent as is");
    assert.deepEqual(sent[2][1], []);
    const [m3, t3] = sent[3];
    assert.ok(m3.args[0].data instanceof Uint8ClampedArray && m3.args[0].data.buffer.byteLength === 12);
    assert.deepEqual(t3, [m3.args[0].data.buffer]);
    assert.equal(image.data.buffer, big.buffer, "the caller's image is untouched");
    assert.deepEqual(m3.args[1], { quality: 50 });
    assert.equal(big.byteLength, 1 << 20, "the caller's buffer is not detached");
    c.api.terminate();
    for (const call of calls) await assert.rejects(call, /worker terminated/);
  }

  // Buffers whose slice() is a view of the same memory (Node's Buffer, its
  // browser polyfills, subclasses): still copied, never moved from the
  // caller. A pooled Buffer cannot be moved at all, yet must still work.
  {
    const w = pair();
    const pool = Buffer.alloc(32 * 24 * 3 + 10, 50);
    const viaBuffer = await w.api.encode({ data: pool.subarray(10), width: 32, height: 24 });
    assert.equal(pool.byteLength, 32 * 24 * 3 + 10, "a Buffer's memory is not moved");
    assert.deepEqual(viaBuffer, web.encode({ data: pool.subarray(10), width: 32, height: 24 }));
    const pooled = Buffer.from(jpeg);
    assert.ok(pooled.byteOffset > 0 || pooled.byteLength < pooled.buffer.byteLength, "from the pool");
    assert.deepEqual(await w.api.info(pooled), web.info(jpeg));
    assert.equal(pooled.byteLength, jpeg.byteLength);
    class Sub extends Uint8Array {
      slice(a, b) {
        return this.subarray(a, b);
      }
    }
    const s = new Sub(jpeg.byteLength + 8);
    s.set(jpeg, 4);
    assert.deepEqual(await w.api.info(s.subarray(4, 4 + jpeg.byteLength)), web.info(jpeg));
    assert.equal(s.byteLength, jpeg.byteLength + 8);
    w.api.terminate();
  }

  // Each property of an options object is read once.
  {
    const sent = [];
    const fake = { postMessage: (m) => sent.push(m), onmessage: null, onmessageerror: null };
    const c = connect(fake, undefined, () => {});
    let reads = 0;
    const image = {
      width: 2,
      height: 2,
      get data() {
        reads++;
        return new Uint8Array(24).subarray(0, 12);
      },
    };
    const call = c.api.encode(image);
    assert.equal(reads, 1);
    assert.equal(sent[1].args[0].data.byteLength, 12);
    c.api.terminate();
    await assert.rejects(call, /worker terminated/);
  }

  // createWorker with a stand-in Worker: options that throw when read
  // reject instead of throwing.
  {
    const entry = await import("smallpeg/worker");
    const started = [];
    globalThis.Worker = class {
      constructor(url, opts) {
        started.push([String(url), opts]);
      }
      postMessage() {}
      terminate() {}
    };
    try {
      const bad = entry.createWorker({
        get wasm() {
          throw new Error("getter");
        },
      });
      await assert.rejects(bad.ready, /^Error: getter$/);
      await assert.rejects(bad.info(jpeg), /getter/);
      const hostile = entry.createWorker(new Proxy({}, { get() { throw new Error("proxy"); } }));
      await assert.rejects(hostile.ready, /proxy/);
      assert.equal(started.length, 0, "no worker started for bad options");
      const ok = entry.createWorker({ wasm: "https://example.com/x.wasm" });
      assert.equal(started.length, 1);
      assert.match(started[0][0], /\/worker-main\.mjs$/);
      assert.deepEqual(started[0][1], { type: "module" });
      ok.terminate();
    } finally {
      delete globalThis.Worker;
    }
  }

  // The entry point itself: Node has no Web Workers.
  const entry = await import("smallpeg/worker");
  assert.throws(() => entry.createWorker(), /^TypeError: smallpeg: createWorker needs Web Workers$/);
  assert.deepEqual(Object.keys(entry), ["createWorker"]);
  console.log("worker protocol: ok");
}

// The same protocol with the functions of smallpeg/jxl, as its
// createWorker() and worker script wire it.
export async function testJxlWorker(jxlWeb, web) {
  function pair(api = jxlWeb) {
    const { port1, port2 } = new MessageChannel();
    serveFns(port2, api, (w) => jxlWeb.init(w), jxlNames);
    return connectFns(
      port1,
      undefined,
      () => {
        port1.close();
        port2.close();
      },
      jxlNames,
    );
  }
  const jpeg = web.encode({ data: new Uint8Array(40 * 30 * 3).map((_, i) => i * 5), width: 40, height: 30 });

  const w = pair();
  assert.deepEqual(Object.keys(w.api).sort(), ["ready", "terminate", "toJxl"]);
  await w.api.ready;
  // Same results as calling the browser build directly; the JPEG is copied.
  assert.deepEqual(await w.api.toJxl(jpeg), jxlWeb.toJxl(jpeg));
  assert.deepEqual(await w.api.toJxl(jpeg, { reconstruction: false }), jxlWeb.toJxl(jpeg, { reconstruction: false }));
  assert.equal(await w.api.toJxl(new Uint8Array([1, 2, 3])), undefined);
  assert.ok(jpeg.byteLength > 0, "the caller's buffer is not detached");
  await assert.rejects(w.api.toJxl(jpeg, { reconstruction: 1 }), (e) => {
    assert.equal(`${e.name}: ${e.message}`, message(() => jxlWeb.toJxl(jpeg, { reconstruction: 1 })));
    return e instanceof TypeError;
  });
  // Only its own functions: the main ones are not served here, whatever
  // the object behind it has.
  w.api.terminate();
  await assert.rejects(w.api.toJxl(jpeg), /^Error: smallpeg: worker terminated$/);
  {
    const { port1, port2 } = new MessageChannel();
    serveFns(port2, { ...jxlWeb, encode: web.encode, init: jxlWeb.init }, (x) => jxlWeb.init(x), jxlNames);
    const c = connect(port1, undefined, () => {
      port1.close();
      port2.close();
    });
    for (const call of [c.api.encode({ data: new Uint8Array(3), width: 1, height: 1 }), c.api.info(jpeg)]) {
      await assert.rejects(call, /^TypeError: smallpeg: no function (encode|info)$/);
    }
    c.api.terminate();
  }

  // The file's bytes are moved out of the worker, not copied.
  {
    const served = [];
    const w = pair({
      ...jxlWeb,
      toJxl: (...a) => {
        const r = jxlWeb.toJxl(...a);
        served.push(r);
        return r;
      },
    });
    const out = await w.api.toJxl(jpeg);
    assert.equal(served[0].data.byteLength, 0, "the file transferred");
    assert.ok(out.data.byteLength > 0 && out.reconstruction === true);
    w.api.terminate();
  }

  // After a WebAssembly trap the worker stops, as the main one does.
  {
    const w = pair({
      ...jxlWeb,
      toJxl: () => {
        throw new WebAssembly.RuntimeError("unreachable");
      },
    });
    await assert.rejects(w.api.toJxl(jpeg), (e) => e.name === "RuntimeError");
    await assert.rejects(w.api.toJxl(jpeg), /the worker stopped after a WebAssembly error \(unreachable\); create a new one$/);
    w.api.terminate();
  }
  console.log("jxl worker protocol: ok");
}

// The entry points themselves, which need a browser: with a stand-in for
// `Worker`, that createWorker() starts its own script (directly, and by
// the same-origin script it falls back to); and with a stand-in for the
// worker's `self`, that the script serves its own functions and no others.
// `cases` is [entry, script, names, a call that works, its direct result].
export async function testWorkerEntries(cases) {
  const real = { Worker: globalThis.Worker, self: globalThis.self, setTimeout: globalThis.setTimeout };
  try {
    for (const [entry, script, fns, call, want] of cases) {
      const { createWorker } = await import(entry);
      // Started directly.
      const started = [];
      globalThis.Worker = class {
        constructor(url, options) {
          started.push([url, options]);
        }
        postMessage() {}
        terminate() {}
      };
      const direct = createWorker();
      assert.deepEqual(Object.keys(direct).sort(), [...fns, "ready", "terminate"].sort(), entry);
      direct.terminate();
      assert.equal(started.length, 1);
      assert.ok(started[0][0] instanceof URL && started[0][0].href.endsWith(`/dist/${script}`), `${entry} starts ${script}`);
      assert.ok(existsSync(fileURLToPath(started[0][0])), `${script} is in the package`);
      assert.deepEqual(started[0][1], { type: "module" });
      // From another origin the first start throws: a same-origin script
      // that imports the same file is started instead.
      const imported = [];
      globalThis.setTimeout = () => 0; // the revoke of the script's URL
      globalThis.Worker = class {
        constructor(url) {
          if (url instanceof URL) throw new Error("SecurityError");
          imported.push(fetch(url).then((r) => r.text()));
        }
        postMessage() {}
        terminate() {}
      };
      createWorker().terminate();
      assert.equal(await imported[0], `import ${JSON.stringify(started[0][0].href)};`, `${entry}: the fallback imports ${script}`);
      globalThis.setTimeout = real.setTimeout;
      // When that fails too, the first reason is given.
      globalThis.Worker = class {
        constructor() {
          throw new Error("no workers here");
        }
      };
      await assert.rejects(createWorker().ready, /^Error: smallpeg: the worker failed to start: no workers here$/);

      // The script: over a port standing in for `self`.
      const { port1, port2 } = new MessageChannel();
      globalThis.self = port2;
      await import(started[0][0].href);
      globalThis.self = real.self;
      const stop = () => {
        port1.close();
        port2.close();
      };
      // Asked for every function of both sets, it answers its own only.
      const all = connectFns(port1, undefined, stop, [...new Set([...names, ...jxlNames])]);
      await all.api.ready;
      assert.deepEqual(await call(all.api), want, `${script} serves its functions`);
      for (const fn of [...names, ...jxlNames].filter((f) => !fns.includes(f))) {
        await assert.rejects(all.api[fn](), new RegExp(`^TypeError: smallpeg: no function ${fn}$`), `${script} does not serve ${fn}`);
      }
      all.api.terminate();
    }
  } finally {
    for (const [k, v] of Object.entries(real)) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
  console.log("worker entry points: ok");
}
