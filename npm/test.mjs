// Smoke test for both builds, through the published entry points: run
// `./npm/build.sh` first, then `node npm/test.mjs`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// By package name, so the exports map is exercised too. The Node build
// is tested before the browser build is even loaded.
const node = await import("smallpeg");
await node.init();

function gradient(w, h, channels) {
  const px = new Uint8Array(w * h * channels);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * channels;
      px[i] = (x * 255) / w;
      if (channels >= 3) {
        px[i + 1] = (y * 255) / h;
        px[i + 2] = 128;
      }
      if (channels === 4) px[i + 3] = 255;
    }
  }
  return px;
}

function run(lib, name) {
  const [width, height] = [96, 64];

  for (const ch of [1, 3, 4]) {
    const image = { data: gradient(width, height, ch), width, height };
    const jpeg = lib.encode(image, { quality: 80 });
    assert.equal(jpeg[0], 0xff, `${name}: SOI for ${ch} channel(s)`);
    assert.equal(jpeg[1], 0xd8);
    // Explicit channels give the same bytes as inferred ones.
    assert.deepEqual(lib.encode({ ...image, channels: ch }, { quality: 80 }), jpeg);
    const i = lib.info(jpeg);
    assert.deepEqual(Object.keys(i).sort(), ["components", "height", "orientation", "progressive", "width"]);
    assert.deepEqual([i.width, i.height, i.components, i.orientation], [width, height, ch === 1 ? 1 : 3, 1]);
    assert.equal(typeof i.progressive, "boolean"); // whichever coding came out smaller
    const img = lib.decode(jpeg);
    assert.equal(img.channels, ch === 1 ? 1 : 3);
    assert.equal(img.data.length, width * height * img.channels);
  }

  // Canvas ImageData carries a Uint8ClampedArray.
  const rgba = gradient(width, height, 4);
  const clamped = new Uint8ClampedArray(rgba.buffer);
  assert.deepEqual(lib.encode({ data: clamped, width, height }), lib.encode({ data: rgba, width, height }));

  // Defaults and full color.
  const rgb = { data: gradient(width, height, 3), width, height };
  const a = lib.encode(rgb);
  assert.deepEqual(lib.encode(rgb, { quality: 80, fullColor: false }), a);
  assert.notDeepEqual(lib.encode(rgb, { fullColor: true }), a);
  assert.deepEqual(lib.encode(rgb, null), a); // null options from JavaScript mean defaults

  // Lossless paths keep pixels.
  const opt = lib.optimize(a);
  assert.deepEqual(lib.decode(opt).data, lib.decode(a).data, `${name}: optimize keeps pixels`);
  assert.equal(lib.info(lib.optimize(a, { baseline: true })).progressive, false);
  const clean = lib.stripMetadata(a);
  assert.deepEqual(lib.decode(clean).data, lib.decode(a).data);

  // upright: an EXIF orientation of 6 (turn 90 degrees clockwise) is
  // applied to the image, and the tag becomes 1.
  const exif = [0x45, 0x78, 0x69, 0x66, 0, 0, 0x4d, 0x4d, 0, 42, 0, 0, 0, 8, 0, 1,
    0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0];
  const tagged = new Uint8Array([0xff, 0xd8, 0xff, 0xe1, 0, exif.length + 2, ...exif, ...a.subarray(2)]);
  assert.equal(lib.info(tagged).orientation, 6);
  const up = lib.upright(tagged);
  assert.deepEqual([lib.info(up).width, lib.info(up).height, lib.info(up).orientation], [height, width, 1]);
  const want = lib.applyOrientation(lib.decode(a), 6).data;
  const got = lib.decode(up).data;
  assert.ok(got.every((v, i) => Math.abs(v - want[i]) <= 1), `${name}: upright keeps pixels`);
  assert.equal(lib.upright(new Uint8Array([0xff, 0xd8])), undefined);

  // Untrusted input: undefined, not an exception.
  assert.equal(lib.decode(new Uint8Array([1, 2, 3])), undefined);
  assert.equal(lib.optimize(new Uint8Array([0xff, 0xd8, 0xff])), undefined);
  assert.equal(lib.info(new Uint8Array(0)), undefined);

  // Pixel helpers return images that chain.
  assert.deepEqual(lib.fitWithin(4000, 3000, 1600, 1600), [1600, 1200]);
  const small = lib.resize(rgb, 48, 32);
  assert.deepEqual([small.width, small.height, small.channels, small.data.length], [48, 32, 3, 48 * 32 * 3]);
  assert.equal(lib.resize(rgb, 48, 32, { linear: true }).data.length, 48 * 32 * 3);
  const cut = lib.crop(rgb, { x: 10, y: 5, width: 20, height: 10 });
  assert.deepEqual([cut.width, cut.height, cut.channels, cut.data.length], [20, 10, 3, 20 * 10 * 3]);
  // The crop starts at (10, 5): its first pixel is that pixel of the source.
  const at = (5 * width + 10) * 3;
  assert.deepEqual([...cut.data.subarray(0, 3)], [...rgb.data.subarray(at, at + 3)]);
  assert.equal(lib.crop(rgb, { x: 90, y: 0, width: 20, height: 10 }), undefined);
  const turned = lib.applyOrientation(rgb, 6);
  assert.deepEqual([turned.width, turned.height, turned.channels], [height, width, 3]);
  assert.ok(lib.encode(lib.resize(lib.decode(a), 32, 24)).length > 0, `${name}: decode → resize → encode`);

  // Results are fresh copies backed by a plain ArrayBuffer.
  for (const out of [a, opt, clean, small.data, cut.data, turned.data, lib.decode(a).data]) {
    assert.ok(out.buffer instanceof ArrayBuffer && out.byteOffset === 0 && out.byteLength === out.buffer.byteLength);
  }
  // Arguments of the wrong kind are TypeErrors, as in 0.13-style calls.
  assert.throws(() => lib.encode({ data: Array.from(rgb.data), width, height }), TypeError);
  assert.throws(() => lib.encode(rgb.data, width, height, 3, 80), TypeError);
  assert.throws(() => lib.encode(rgb, 80), TypeError);
  assert.throws(() => lib.resize(rgb, 10, 10, true), TypeError);
  assert.throws(() => lib.encode(rgb, { fullColor: "yes" }), TypeError);
  assert.throws(() => lib.optimize(a, { baseline: 1 }), TypeError);
  assert.throws(() => lib.info("abc"), TypeError);
  assert.throws(() => lib.info(null), TypeError);
  assert.throws(() => lib.decode(a.buffer), TypeError);
  assert.throws(() => lib.crop(rgb, null), TypeError);
  assert.throws(() => lib.crop(rgb), TypeError);
  assert.throws(() => lib.encode({ ...rgb, channels: "3" }), TypeError);
  assert.throws(() => lib.applyOrientation(rgb, "6"), TypeError);
  assert.throws(() => lib.resize(rgb, 0xffffffff, 1), /from 1 to 65535/);
  assert.throws(() => lib.resize(rgb, 65536, 1), RangeError);
  // A thin, very long result is cheap in pixels but not in filter tables.
  assert.throws(() => lib.resize({ data: new Uint8Array(3), width: 1, height: 1 }, 100000000, 1), RangeError);
  assert.throws(() => lib.resize(rgb, 70000, 70000), RangeError);
  // The working image (new width by old height) is bounded too: a tall
  // source made very wide must not run the wasm out of memory.
  const tall = { data: new Uint8Array(600000), width: 1, height: 600000, channels: 1 };
  assert.throws(() => lib.resize(tall, 1000, 1), (e) => e instanceof RangeError && /working space/.test(e.message));
  assert.equal(lib.resize(tall, 100, 1).data.length, 100, `${name}: usable after the refusal`);
  assert.throws(() => lib.resize(rgb, Object.create(null), 1), /got \[object Object\]/);
  // Odd values still give the intended error and a readable message.
  assert.throws(() => lib.encode(rgb, { quality: NaN }), /quality .* got NaN/);
  assert.throws(() => lib.resize(rgb, 10n, 1), /width .* got 10/);
  assert.throws(() => lib.resize(rgb, "10", 1), /got "10"/);

  // Argument mistakes are JavaScript errors, and the instance keeps working.
  assert.throws(() => lib.encode(rgb, { quality: 0 }), RangeError);
  assert.throws(() => lib.encode(rgb, { quality: 300 }), /quality/);
  assert.throws(() => lib.encode(rgb, []), TypeError);
  assert.throws(() => lib.encode(rgb, { fullColor: null }), TypeError);
  assert.throws(() => lib.resize(rgb, 2.5, 10), /integer/);
  assert.throws(() => lib.resize(rgb, -1, 10), /integer/);
  assert.throws(() => lib.resize(rgb, 0, 10), /from 1/);
  assert.throws(() => lib.crop(rgb, { x: 1.5, y: 0, width: 2, height: 2 }), /integer/);
  assert.throws(() => lib.fitWithin(-1, 10, 5, 5), /integer/);
  assert.throws(() => lib.fitWithin(0, 10, 5, 5), /from 1/);
  assert.throws(() => lib.crop({ ...rgb, channels: 2 }, { x: 0, y: 0, width: 1, height: 1 }), /channels/);
  assert.throws(() => lib.resize({ ...rgb, channels: 3.5 }, 2, 2), /channels/);
  assert.throws(() => lib.encode({ ...rgb, height: height + 1 }), /channels/);
  assert.throws(() => lib.encode({ ...rgb, channels: 4 }), (e) => e instanceof RangeError && /^smallpeg: expected/.test(e.message));
  assert.throws(() => lib.encode({ data: new Uint8Array(3 * 70000), width: 70000, height: 1 }), /65535/);
  assert.throws(() => lib.resize({ data: rgba, width, height }, 10, 10, { linear: true }), /linear/);
  assert.ok(lib.encode(rgb).length > 0, `${name}: usable after an error`);

  console.log(`${name}: ok`);
}

run(node, "node");

const web = await import("smallpeg/web");
// Node has no fetch for file: URLs, so hand the browser build the bytes.
await web.init(readFileSync(new URL("./web/smallpeg_wasm_bg.wasm", import.meta.url)));
await web.init(); // already loaded: same promise, no refetch
run(web, "web");

// The worker entry point's protocol, over a MessageChannel.
const { testWorker } = await import("./test-worker.mjs");
await testWorker(web, web.encode({ data: new Uint8Array(40 * 30 * 3).fill(90), width: 40, height: 30 }));

// Both builds export the same names.
assert.deepEqual(Object.keys(node).sort(), Object.keys(web).sort());

// smallpeg/jxl: entry points of its own, over a wasm module of its own.
function runJxl(lib, name) {
  const [width, height] = [96, 64];
  const jpeg = node.encode({ data: gradient(width, height, 3), width, height });
  const out = lib.toJxl(jpeg);
  assert.deepEqual(Object.keys(out).sort(), ["data", "reconstruction"]);
  assert.equal(out.reconstruction, true, `${name}: the JPEG can be rebuilt`);
  // A JPEG XL container, which is what holds the reconstruction data.
  assert.deepEqual([...out.data.subarray(0, 8)], [0, 0, 0, 12, 0x4a, 0x58, 0x4c, 0x20]);
  assert.ok(out.data instanceof Uint8Array && out.data.buffer instanceof ArrayBuffer);
  assert.ok(out.data.length < jpeg.length, `${name}: smaller than the JPEG`);
  // Without it: a bare codestream, smaller still.
  const bare = lib.toJxl(jpeg, { reconstruction: false });
  assert.equal(bare.reconstruction, false);
  assert.deepEqual([bare.data[0], bare.data[1]], [0xff, 0x0a]);
  assert.ok(bare.data.length < out.data.length);
  // Options left out, null or undefined mean the defaults.
  assert.deepEqual(lib.toJxl(jpeg, null), out);
  assert.deepEqual(lib.toJxl(jpeg, { reconstruction: undefined }), out);
  // The bytes are the caller's own: a whole buffer, not a view into memory.
  assert.equal(out.data.byteOffset, 0);
  assert.equal(out.data.buffer.byteLength, out.data.length);
  // Grayscale converts too.
  const gray = node.encode({ data: gradient(width, height, 1), width, height });
  assert.equal(lib.toJxl(gray).reconstruction, true);
  // The flag is what happened, not what was asked for: a frame header with
  // sampling factors the format cannot carry converts as an image only.
  const odd = gray.slice();
  const sof = odd.findIndex((b, i) => b === 0xff && (odd[i + 1] === 0xc0 || odd[i + 1] === 0xc2));
  assert.equal(odd[sof + 11], 0x11);
  odd[sof + 11] = 0x41;
  const imageOnly = lib.toJxl(odd);
  assert.equal(imageOnly.reconstruction, false, `${name}: asked for, but not possible`);
  assert.deepEqual([imageOnly.data[0], imageOnly.data[1]], [0xff, 0x0a]);
  assert.deepEqual(imageOnly, lib.toJxl(odd, { reconstruction: false }));
  // Not a JPEG this reads: undefined, like the other functions.
  assert.equal(lib.toJxl(new Uint8Array([1, 2, 3, 4])), undefined);
  assert.equal(lib.toJxl(jpeg.subarray(0, jpeg.length >> 1)), undefined);
  // Nor a file over 512 MB, which is refused before it is copied in.
  const huge = new Uint8Array(512 * 1024 * 1024 + 1);
  huge.set(jpeg);
  assert.equal(lib.toJxl(huge), undefined, `${name}: over the size limit`);
  assert.throws(() => lib.toJxl(huge, { reconstruction: 1 }), { name: "TypeError" });
  assert.throws(() => lib.toJxl("not bytes"), { name: "TypeError", message: /^smallpeg: the JPEG/ });
  assert.throws(() => lib.toJxl(jpeg, { reconstruction: 1 }), { name: "TypeError", message: /^smallpeg: reconstruction/ });
  assert.throws(() => lib.toJxl(jpeg, []), { name: "TypeError", message: /^smallpeg: options/ });
  assert.ok(lib.toJxl(jpeg), `${name}: usable after an error`);
  console.log(`${name}: ok`);
  return out;
}
const jxlNode = await import("smallpeg/jxl");
await jxlNode.init();
const fromNode = runJxl(jxlNode, "jxl node");
const jxlWeb = await import("smallpeg/jxl/web");
await jxlWeb.init(readFileSync(new URL("./jxl-web/smallpeg_jxl_wasm_bg.wasm", import.meta.url)));
assert.deepEqual(runJxl(jxlWeb, "jxl web"), fromNode, "the two jxl builds give the same file");
assert.deepEqual(Object.keys(jxlNode).sort(), Object.keys(jxlWeb).sort());
assert.deepEqual(Object.keys(jxlNode).sort(), ["default", "init", "initSync", "toJxl"]);
// It stays out of the main entry points.
assert.equal(node.toJxl, undefined);
assert.equal(web.toJxl, undefined);

// Each way of loading the wasm, in a fresh process: once the glue has
// loaded, it ignores later loads, so one process would hide a broken form.
const initCases = [
  "web:before-init",
  "web:init-bytes",
  "web:init-module",
  "web:init-response",
  "web:init-promise",
  "web:init-legacy-object",
  "web:init-default-export",
  "web:initSync-bytes",
  "web:initSync-module",
  "web:initSync-legacy-object",
  "web:init-retry",
  "web:init-after-initSync",
  "web:initSync-after-init",
  "web:initSync-during-init",
  "web:init-wrong-object",
  "web:require",
  "node:require",
  "node:browser-style-calls",
  "jxl:before-init",
  "jxl:main-init-is-not-enough",
  "jxl:init-bytes",
  "jxl:init-module",
  "jxl:init-response",
  "jxl:initSync-bytes",
  "jxl:init-retry",
  "jxl:initSync-during-init",
  "jxl:init-wrong-object",
  "jxl:require",
  "jxl-node:require",
  "jxl-node:browser-style-calls",
];
const script = fileURLToPath(new URL("./test-init.mjs", import.meta.url));
for (const c of initCases) {
  // stderr must stay empty: the glue still accepts some argument shapes it
  // has deprecated, and only says so with a console warning.
  const r = spawnSync(process.execPath, [script, c], { encoding: "utf8" });
  assert.equal(r.status, 0, `${c}: ${r.stderr}`);
  assert.equal(r.stdout.trim(), `${c} ok`);
  assert.equal(r.stderr, "", `${c} wrote to stderr`);
}
console.log(`init forms: ${initCases.length} ok`);

// The two entry points declare and document their functions identically
// (everything from the shared type exports on).
function api(entry) {
  const lines = readFileSync(new URL(`./dist/${entry}.d.mts`, import.meta.url), "utf8").split("\n");
  return lines.slice(lines.findIndex((l) => l.startsWith("export type {")));
}
assert.deepEqual(api("node"), api("web"), "node.d.mts and web.d.mts differ in the shared API");
assert.deepEqual(api("jxl-node"), api("jxl-web"), "jxl-node.d.mts and jxl-web.d.mts differ in the shared API");
