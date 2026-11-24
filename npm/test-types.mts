// Type test: compiled with `tsc -p` test-types.tsconfig.json,
// test-types-node.tsconfig.json and test-types-bundler.tsconfig.json (no
// output) to check the
// published declarations read the way the README says. Lines under an
// expect-error directive must fail to type-check. No DOM types here: the bundler
// config has no DOM lib (see test-types-dom.mts for the rest).

import initDefault, {
  crop,
  decode,
  encode,
  fitWithin,
  info,
  init,
  initSync,
  optimize,
  resize,
  upright,
} from "smallpeg";
import type { Bytes, Channels, Image, ImageInput, JpegInfo } from "smallpeg";
import * as web from "smallpeg/web";
import { createWorker, type WorkerApi } from "smallpeg/worker";
import initJxl, { toJxl, type JxlOptions, type JxlOutput } from "smallpeg/jxl";
import type { Bytes as JxlBytes } from "smallpeg/jxl";
import * as jxlWeb from "smallpeg/jxl/web";
import { createWorker as createJxlWorker, type JxlWorkerApi } from "smallpeg/jxl/worker";
import type { JxlOutput as WorkerJxlOutput, WorkerOptions as JxlWorkerOptions } from "smallpeg/jxl/worker";
import type { Image as WorkerImage } from "smallpeg/worker";

declare const jpeg: Uint8Array;

await init();
// Browser-style calls type-check against either entry point.
await init("https://example.com/smallpeg_wasm_bg.wasm");
await initDefault();
initSync(new Uint8Array(0));

const rgb: ImageInput = { data: new Uint8Array(12), width: 2, height: 2 };
const a: Bytes = encode(rgb);
const b: Uint8Array<ArrayBuffer> = encode({ ...rgb, channels: 3 }, { quality: 90, fullColor: true });
// @ts-expect-error channels must be 1, 3 or 4
encode({ data: new Uint8Array(8), width: 2, height: 2, channels: 2 });
// @ts-expect-error quality goes in the options object
encode(rgb, 80);

// Results that can fail say so.
const maybe: Bytes | undefined = optimize(jpeg, { baseline: true });
const turned: Bytes | undefined = upright(jpeg);
// @ts-expect-error optimize can return undefined
const sure: Uint8Array = optimize(jpeg);

const i: JpegInfo | undefined = info(jpeg);
if (i) {
  const c: 1 | 2 | 3 | 4 = i.components;
  const o: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 = i.orientation;
  const p: boolean = i.progressive;
  void [c, o, p];
}

// Images chain from one call to the next.
const img: Image | undefined = decode(jpeg);
if (img) {
  const ch: Channels = img.channels;
  const [w, h]: [number, number] = fitWithin(img.width, img.height, 1600, 1600);
  const small: Image = resize(img, w, h, { linear: true });
  const part: Image | undefined = crop(small, { x: 0, y: 0, width: 10, height: 10 });
  encode(small);
  void [ch, part];
  // @ts-expect-error no free(): results are plain objects
  img.free();
}

await web.init();
await web.init("https://example.com/smallpeg_wasm_bg.wasm");
await web.init(new Uint8Array(0));
await web.init({ module_or_path: "https://example.com/smallpeg_wasm_bg.wasm" });
await web.default();
web.initSync(new Uint8Array(0));
web.initSync({ module: new Uint8Array(0) });
web.encode(rgb, { quality: 75 });

// smallpeg/jxl: its own entry points; the result says whether the JPEG
// can be rebuilt.
await initJxl();
const jxlOptions: JxlOptions = { reconstruction: false };
const jxl: JxlOutput | undefined = toJxl(jpeg, jxlOptions);
if (jxl) {
  const file: JxlBytes = jxl.data;
  const same: Bytes = file; // the same type as the main entry points'
  const rebuilds: boolean = jxl.reconstruction;
  void [same, rebuilds];
}
// @ts-expect-error toJxl can return undefined
const sureJxl: JxlOutput = toJxl(jpeg);
// @ts-expect-error reconstruction is a boolean
toJxl(jpeg, { reconstruction: 1 });
// @ts-expect-error the option goes in an object
toJxl(jpeg, false);
await jxlWeb.init("https://example.com/smallpeg_jxl_wasm_bg.wasm");
await jxlWeb.default(new Uint8Array(0));
jxlWeb.initSync(new Uint8Array(0));
jxlWeb.toJxl(jpeg);
void sureJxl;

// The worker API: same arguments, promised results.
const w: WorkerApi = createWorker({ wasm: "https://example.com/smallpeg_wasm_bg.wasm" });
createWorker();
await w.ready;
const wjpeg: Bytes = await w.encode(rgb, { quality: 90 });
const wmaybe: Bytes | undefined = await w.optimize(wjpeg, { baseline: true });
const wimg: WorkerImage | undefined = await w.decode(wjpeg);
const wsize: [number, number] = await w.fitWithin(4000, 3000, 1000, 1000);
const wsame: Image = await w.resize(rgb, 2, 2);
w.terminate();
// @ts-expect-error results are promises
const wsync: Bytes = w.encode(rgb);
// @ts-expect-error options are checked as for encode
w.encode(rgb, { qualty: 90 });
// @ts-expect-error the worker loads the wasm itself
w.init();
// @ts-expect-error decode can give undefined
const wsure: Image = await w.decode(wjpeg);

// The JPEG XL worker: toJxl alone, promised.
const jxlWorkerOptions: JxlWorkerOptions = { wasm: "https://example.com/smallpeg_jxl_wasm_bg.wasm" };
const jw: JxlWorkerApi = createJxlWorker(jxlWorkerOptions);
createJxlWorker();
await jw.ready;
const wjxl: WorkerJxlOutput | undefined = await jw.toJxl(jpeg, { reconstruction: false });
const wjxlSame: JxlOutput | undefined = await jw.toJxl(jpeg);
jw.terminate();
// @ts-expect-error results are promises
const wjxlSync: JxlOutput | undefined = jw.toJxl(jpeg);
// @ts-expect-error options are checked as for toJxl
jw.toJxl(jpeg, { reconstructon: false });
// @ts-expect-error the main functions are on the other worker
jw.encode(rgb);
// @ts-expect-error and toJxl is not on the main one
w.toJxl(jpeg);
// @ts-expect-error the worker loads the wasm itself
jw.init();

export { a, b, maybe, sure, wmaybe, wimg, wsize, wsame, wsync, wsure, wjxl, wjxlSame, wjxlSync };
