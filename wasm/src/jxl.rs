//! The `smallpeg/jxl` entry point: JPEG to JPEG XL.

use wasm_bindgen::prelude::*;

/// A JPEG XL file and whether it can give the JPEG back. The TypeScript
/// layer reads the flag, moves the bytes out with `take_data`, and frees
/// it.
#[wasm_bindgen]
pub struct Jxl {
    data: Vec<u8>,
    reconstruction: bool,
}

#[wasm_bindgen]
impl Jxl {
    #[wasm_bindgen(getter)]
    pub fn reconstruction(&self) -> bool {
        self.reconstruction
    }

    /// Moves the bytes out (no copy on the Rust side); a second call
    /// returns an empty array.
    pub fn take_data(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }
}

/// Largest frame converted. The conversion takes up to about 55 bytes per
/// pixel, and a WebAssembly module can address 4 GB: past that it traps
/// and is left unusable. At this size the worst case measured (noise at
/// quality 100 and full chroma) took 2.8 GB.
const MAX_PIXELS: u64 = 50_000_000;

/// Recompress a JPEG as JPEG XL without changing its coefficients;
/// `reconstruction` includes what rebuilds the JPEG file.
#[wasm_bindgen]
pub fn to_jxl(jpeg: &[u8], reconstruction: bool) -> Option<Jxl> {
    let options = smallpeg::JxlOptions::default()
        .limits(smallpeg::Limits::default().max_pixels(MAX_PIXELS))
        .reconstruction(reconstruction);
    smallpeg::jpeg_to_jxl_with(jpeg, &options).map(|out| Jxl {
        data: out.data,
        reconstruction: out.reconstruction,
    })
}
