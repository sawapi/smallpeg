// The typed API shared by the Node and browser entry points. The
// wasm-bindgen glue is an implementation detail: its result classes hold
// wasm memory, so they are copied into plain objects here and freed.

/**
 * Bytes this package hands back. They are always fresh copies backed by a
 * plain `ArrayBuffer`, so they work as a `BlobPart`, and 4-channel pixels
 * can be viewed as the `Uint8ClampedArray` that `new ImageData` takes.
 */
export type Bytes = Uint8Array<ArrayBuffer>;

/** 1 (gray), 3 (RGB) or 4 (RGBA, or any four-channel layout). */
export type Channels = 1 | 3 | 4;

/**
 * Pixels to encode, resize, crop or turn: `width * height * channels`
 * bytes, interleaved, top row first. A canvas `ImageData` fits as is.
 */
export interface ImageInput {
  data: Uint8Array | Uint8ClampedArray;
  width: number;
  height: number;
  /** Worked out from the length of `data` when left out. */
  channels?: Channels | undefined;
}

/** Pixels returned by `decode`, `resize`, `crop` and `applyOrientation`. */
export interface Image {
  data: Bytes;
  width: number;
  height: number;
  channels: Channels;
}

/** Options for `encode`. Every field is optional. */
export interface EncodeOptions {
  /** An integer from 1 to 100. Default 80. */
  quality?: number | undefined;
  /**
   * Keep chroma at full resolution (4:4:4) instead of halving it (4:2:0):
   * sharper colored text and lines, larger files. Default false.
   */
  fullColor?: boolean | undefined;
}

/** Options for `optimize`. */
export interface OptimizeOptions {
  /**
   * Write baseline sequential output instead of progressive, for targets
   * that dislike progressive JPEGs (PDF embedding, for example).
   * Default false.
   */
  baseline?: boolean | undefined;
}

/** Options for `resize`. */
export interface ResizeOptions {
  /**
   * Filter in linear light, which keeps fine light-and-dark detail from
   * darkening when reducing. Assumes sRGB; 1 or 3 channels only.
   * Default false.
   */
  linear?: boolean | undefined;
}

/** A rectangle in pixels; `x` and `y` are its top-left corner. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** EXIF orientation: 1 is upright; 5 to 8 swap width and height. */
export type Orientation = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;

/** Frame-header facts about a JPEG, returned by `info`. */
export interface JpegInfo {
  width: number;
  height: number;
  /** 1 (gray), 3 (color) or 4 (CMYK / YCCK); 2 in rare files. */
  components: 1 | 2 | 3 | 4;
  /** True for progressive files. */
  progressive: boolean;
  /** EXIF orientation, 1 to 8 (1 when the file has none). */
  orientation: Orientation;
}

// The parts of the wasm-bindgen glue used here, written out so the
// published declarations do not depend on the glue's. The entry points
// pass the real glue in, so a mismatch fails to compile there.
interface Freeable {
  free(): void;
}
interface GlueImage extends Freeable {
  readonly width: number;
  readonly height: number;
  readonly channels: number;
  take_pixels(): Uint8Array;
}
interface GlueInfo extends Freeable {
  readonly width: number;
  readonly height: number;
  readonly components: number;
  readonly progressive: boolean;
  readonly orientation: number;
}
// Property (not method) syntax, so parameters are compared strictly. The
// entry points also check that the parameter lists match exactly (see
// `ParamsMatch`), so a glue function that lost a parameter fails too.
export interface Bindings {
  encode: (
    pixels: Uint8Array,
    width: number,
    height: number,
    channels: number,
    quality?: number | null,
    full_color?: boolean | null,
  ) => Uint8Array;
  optimize: (jpeg: Uint8Array, baseline: boolean) => Uint8Array | undefined;
  upright: (jpeg: Uint8Array) => Uint8Array | undefined;
  strip_metadata: (jpeg: Uint8Array) => Uint8Array | undefined;
  info: (jpeg: Uint8Array) => GlueInfo | undefined;
  decode: (jpeg: Uint8Array) => GlueImage | undefined;
  apply_orientation: (
    pixels: Uint8Array,
    width: number,
    height: number,
    channels: number,
    orientation: number,
  ) => GlueImage;
  resize: (
    pixels: Uint8Array,
    width: number,
    height: number,
    channels: number,
    new_width: number,
    new_height: number,
    linear: boolean,
  ) => Uint8Array;
  crop: (
    pixels: Uint8Array,
    width: number,
    height: number,
    channels: number,
    x: number,
    y: number,
    crop_width: number,
    crop_height: number,
  ) => Uint8Array | undefined;
  fit_within: (width: number, height: number, max_width: number, max_height: number) => Uint32Array;
}

type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;

/**
 * For each glue function, whether its parameter list is exactly the one in
 * `Bindings` (both directions, so a parameter the glue gained, lost or
 * changed shows up). The entry points assert it is all `true`.
 */
export type ParamsMatch<G extends Pick<Bindings, keyof Bindings>> = {
  [K in keyof Bindings]: Same<Parameters<G[K]>, Parameters<Bindings[K]>>;
};

/** The functions both entry points export. */
export interface Api {
  encode(image: ImageInput, options?: EncodeOptions): Bytes;
  optimize(jpeg: Uint8Array, options?: OptimizeOptions): Bytes | undefined;
  upright(jpeg: Uint8Array): Bytes | undefined;
  stripMetadata(jpeg: Uint8Array): Bytes | undefined;
  info(jpeg: Uint8Array): JpegInfo | undefined;
  decode(jpeg: Uint8Array): Image | undefined;
  applyOrientation(image: ImageInput, orientation: number): Image;
  resize(image: ImageInput, width: number, height: number, options?: ResizeOptions): Image;
  crop(image: ImageInput, rect: Rect): Image | undefined;
  fitWithin(
    width: number,
    height: number,
    maxWidth: number,
    maxHeight: number,
  ): [width: number, height: number];
}

const MAX_U32 = 0xffff_ffff;

/**
 * Largest resize output and working image (new width by old height), the
 * same 100 megapixels the decoder accepts.
 */
const MAX_PIXELS = 100_000_000;

/** Largest resize width or height, as for a JPEG. */
const MAX_SIDE = 65535;

/** A value for an error message: strings quoted, everything else as is. */
export function show(v: unknown): string {
  if (typeof v === "string") return JSON.stringify(v);
  if (Array.isArray(v)) return "an array";
  try {
    return String(v);
  } catch {
    return Object.prototype.toString.call(v); // e.g. a null-prototype object
  }
}

/** Throws unless `v` is an integer in `min..=max`. */
function int(name: string, v: number, min: number, max = MAX_U32): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < min || v > max) {
    const msg = `smallpeg: ${name} must be an integer from ${min} to ${max}, got ${show(v)}`;
    throw typeof v === "number" ? new RangeError(msg) : new TypeError(msg);
  }
  return v;
}

/** JPEG input: a `Uint8Array` (a Node `Buffer` is one). */
export function jpegBytes(jpeg: Uint8Array): Uint8Array {
  if (!(jpeg instanceof Uint8Array)) {
    throw new TypeError("smallpeg: the JPEG must be a Uint8Array (a Node Buffer is fine)");
  }
  return jpeg;
}

/** Pixel input: a `Uint8Array`, or a canvas's `Uint8ClampedArray`. */
function pixelBytes(data: Uint8Array | Uint8ClampedArray): Uint8Array {
  if (data instanceof Uint8Array) return data;
  if (data instanceof Uint8ClampedArray) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  throw new TypeError("smallpeg: image data must be a Uint8Array or Uint8ClampedArray");
}

/** `options` as an object: `undefined` and `null` mean defaults. */
export function opts<T extends object>(options: T | null | undefined): Partial<T> {
  if (options === undefined || options === null) return {};
  if (typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError(`smallpeg: options must be an object, got ${show(options)}`);
  }
  return options;
}

/** A boolean option; left out means false. */
function flag(value: unknown, name: string): boolean {
  if (value === undefined) return false;
  if (typeof value !== "boolean") {
    throw new TypeError(`smallpeg: ${name} must be a boolean, got ${show(value)}`);
  }
  return value;
}

/** The glue's byte results are copies out of WebAssembly memory. */
export function owned(data: Uint8Array): Bytes {
  return data as Bytes;
}

function channelsOf(image: ImageInput): Channels {
  if (image.channels !== undefined) {
    const c: unknown = image.channels;
    if (c === 1 || c === 3 || c === 4) return c;
    const msg = `smallpeg: channels must be 1, 3 or 4, got ${show(c)}`;
    throw typeof c === "number" ? new RangeError(msg) : new TypeError(msg);
  }
  const area = Number.isInteger(image.width) && Number.isInteger(image.height) ? image.width * image.height : 0;
  const c = area > 0 ? image.data.length / area : 0;
  if (c === 1 || c === 3 || c === 4) return c;
  throw new RangeError(
    `smallpeg: ${image.data.length} bytes is not 1, 3 or 4 channels of ${image.width}x${image.height}`,
  );
}

/** The image's fields, checked, in the order the glue takes them. */
function unpack(image: ImageInput): [Uint8Array, number, number, Channels] {
  if (typeof image !== "object" || image === null) {
    throw new TypeError("smallpeg: the image must be an object { data, width, height, channels? }");
  }
  const data = pixelBytes(image.data);
  const width = int("width", image.width, 1);
  const height = int("height", image.height, 1);
  const channels = channelsOf(image);
  if (data.length !== width * height * channels) {
    throw new RangeError(
      `smallpeg: expected ${width * height * channels} bytes for ${width}x${height} with ${channels} channel(s), got ${data.length}`,
    );
  }
  return [data, width, height, channels];
}

function take(img: GlueImage): Image {
  try {
    return {
      data: owned(img.take_pixels()),
      width: img.width,
      height: img.height,
      channels: img.channels as Channels,
    };
  } finally {
    img.free();
  }
}

export function makeApi(glue: Bindings, ensureReady: () => void = () => {}): Api {
  function encode(image: ImageInput, options?: EncodeOptions): Bytes {
    ensureReady();
    const args = unpack(image);
    const o = opts(options);
    const quality = o.quality === undefined ? undefined : int("quality", o.quality, 1, 100);
    return owned(glue.encode(...args, quality, flag(o.fullColor, "fullColor")));
  }

  function optimize(jpeg: Uint8Array, options?: OptimizeOptions): Bytes | undefined {
    ensureReady();
    const input = jpegBytes(jpeg);
    const out = glue.optimize(input, flag(opts(options).baseline, "baseline"));
    return out && owned(out);
  }

  function upright(jpeg: Uint8Array): Bytes | undefined {
    ensureReady();
    const out = glue.upright(jpegBytes(jpeg));
    return out && owned(out);
  }

  function stripMetadata(jpeg: Uint8Array): Bytes | undefined {
    ensureReady();
    const out = glue.strip_metadata(jpegBytes(jpeg));
    return out && owned(out);
  }

  function info(jpeg: Uint8Array): JpegInfo | undefined {
    ensureReady();
    const i = glue.info(jpegBytes(jpeg));
    if (!i) return undefined;
    try {
      return {
        width: i.width,
        height: i.height,
        components: i.components as 1 | 2 | 3 | 4,
        progressive: i.progressive,
        orientation: i.orientation as Orientation,
      };
    } finally {
      i.free();
    }
  }

  function decode(jpeg: Uint8Array): Image | undefined {
    ensureReady();
    const img = glue.decode(jpegBytes(jpeg));
    return img ? take(img) : undefined;
  }

  function applyOrientation(image: ImageInput, orientation: number): Image {
    ensureReady();
    if (typeof orientation !== "number") {
      throw new TypeError(`smallpeg: orientation must be a number, got ${show(orientation)}`);
    }
    const o = Number.isInteger(orientation) && orientation >= 1 && orientation <= 8 ? orientation : 1;
    return take(glue.apply_orientation(...unpack(image), o));
  }

  function resize(image: ImageInput, width: number, height: number, options?: ResizeOptions): Image {
    ensureReady();
    const args = unpack(image);
    // Each side is capped like a JPEG's: the filter keeps a table entry per
    // output column and row, so a thin, very long result would otherwise
    // cost far more than its pixel count.
    const w = int("width", width, 1, MAX_SIDE);
    const h = int("height", height, 1, MAX_SIDE);
    if (w * h > MAX_PIXELS) {
      throw new RangeError(`smallpeg: ${w}x${h} is over the ${MAX_PIXELS / 1e6} megapixel limit for resize`);
    }
    // The first pass makes a new-width by old-height image.
    if (w * args[2] > MAX_PIXELS) {
      throw new RangeError(
        `smallpeg: resizing to ${w}x${h} from a height of ${args[2]} needs ${w}x${args[2]} pixels of working space, over the ${MAX_PIXELS / 1e6} megapixel limit`,
      );
    }
    const data = glue.resize(...args, w, h, flag(opts(options).linear, "linear"));
    return { data: owned(data), width: w, height: h, channels: args[3] };
  }

  function crop(image: ImageInput, rect: Rect): Image | undefined {
    ensureReady();
    const args = unpack(image);
    if (typeof rect !== "object" || rect === null) {
      throw new TypeError("smallpeg: the rectangle must be an object { x, y, width, height }");
    }
    const [x, y] = [int("x", rect.x, 0), int("y", rect.y, 0)];
    const [w, h] = [int("width", rect.width, 0), int("height", rect.height, 0)];
    const data = glue.crop(...args, x, y, w, h);
    return data ? { data: owned(data), width: w, height: h, channels: args[3] } : undefined;
  }

  function fitWithin(
    width: number,
    height: number,
    maxWidth: number,
    maxHeight: number,
  ): [width: number, height: number] {
    ensureReady();
    const [w, h] = glue.fit_within(
      int("width", width, 1),
      int("height", height, 1),
      int("maxWidth", maxWidth, 1),
      int("maxHeight", maxHeight, 1),
    );
    return [w!, h!];
  }

  return { encode, optimize, upright, stripMetadata, info, decode, applyOrientation, resize, crop, fitWithin };
}
