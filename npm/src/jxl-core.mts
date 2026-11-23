// The typed API of the `smallpeg/jxl` entry points, which have a wasm
// module of their own. Experimental: it may change in any release.

import { type Bytes, jpegBytes, opts, owned, show } from "./core.mjs";

export type { Bytes };

/** Options for `toJxl`. */
export interface JxlOptions {
  /**
   * Include what a JPEG XL decoder needs to write the original JPEG file
   * back byte for byte: a few hundred bytes for most files, plus any
   * segments and trailing data the file has beyond the image, its color
   * profile, Exif and XMP. Default true. Without it the image is the
   * same, but the file it came from cannot be had back.
   */
  reconstruction?: boolean | undefined;
}

/** What `toJxl` returns. */
export interface JxlOutput {
  /** The JPEG XL file. */
  data: Bytes;
  /**
   * Whether the file holds what rebuilds the original JPEG byte for
   * byte. False when the option is off, and when the format cannot
   * describe the JPEG (the image is carried over all the same).
   */
  reconstruction: boolean;
}

interface GlueJxl {
  free(): void;
  readonly reconstruction: boolean;
  take_data(): Uint8Array;
}
// Property (not method) syntax, so parameters are compared strictly.
export interface JxlBindings {
  to_jxl: (jpeg: Uint8Array, reconstruction: boolean) => GlueJxl | undefined;
}

type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;

/**
 * For each glue function, whether its parameter list is exactly the one
 * in `JxlBindings`. The entry points assert it is all `true`.
 */
export type JxlParamsMatch<G extends Pick<JxlBindings, keyof JxlBindings>> = {
  [K in keyof JxlBindings]: Same<Parameters<G[K]>, Parameters<JxlBindings[K]>>;
};

/** The functions both entry points export. */
export interface JxlApi {
  toJxl(jpeg: Uint8Array, options?: JxlOptions): JxlOutput | undefined;
}

export function makeJxlApi(glue: JxlBindings, ensureReady: () => void = () => {}): JxlApi {
  function toJxl(jpeg: Uint8Array, options?: JxlOptions): JxlOutput | undefined {
    ensureReady();
    const input = jpegBytes(jpeg);
    const want: unknown = opts(options).reconstruction;
    if (want !== undefined && typeof want !== "boolean") {
      throw new TypeError(`smallpeg: reconstruction must be a boolean, got ${show(want)}`);
    }
    const out = glue.to_jxl(input, want ?? true);
    if (!out) return undefined;
    try {
      return { data: owned(out.take_data()), reconstruction: out.reconstruction };
    } finally {
      out.free();
    }
  }

  return { toJxl };
}
