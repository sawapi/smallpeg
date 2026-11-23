//! wasm-bindgen bindings behind the npm package. They are an internal
//! layer: the package's TypeScript API (npm/src) is what users call, and
//! it converts these results into plain JavaScript values.
//!
//! Every entry point still validates its arguments and reports a mismatch
//! as a JavaScript `Error` instead of letting the library's assertions trap
//! the WebAssembly instance. Functions that take untrusted JPEG bytes
//! return `undefined` for input they cannot handle, like the Rust API's
//! `None`.
//!
//! The crate builds two WebAssembly modules, so that code only one entry
//! point needs is not loaded by the others: the default build is the main
//! API (`smallpeg`, `smallpeg/web`, `smallpeg/worker`), and the build with
//! the `jxl` feature holds only the JPEG XL function (`smallpeg/jxl`).

#[cfg(not(feature = "jxl"))]
mod api;
#[cfg(feature = "jxl")]
mod jxl;
