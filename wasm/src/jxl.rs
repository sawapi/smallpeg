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

/// Recompress a JPEG as JPEG XL without changing its coefficients;
/// `reconstruction` includes what rebuilds the JPEG file.
#[wasm_bindgen]
pub fn to_jxl(jpeg: &[u8], reconstruction: bool) -> Option<Jxl> {
    // The default limit of 100 megapixels stands: the worst case measured
    // at that size (noise at quality 100 and full chroma) takes about 3 GB
    // of the 4 GB a WebAssembly module can address.
    let options = smallpeg::JxlOptions::default().reconstruction(reconstruction);
    smallpeg::jpeg_to_jxl_with(jpeg, &options).map(|out| Jxl {
        data: out.data,
        reconstruction: out.reconstruction,
    })
}
