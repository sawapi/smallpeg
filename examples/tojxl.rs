//! Recompress a JPEG as JPEG XL: `tojxl in.jpg out.jxl`.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let out = smallpeg::jpeg_to_jxl(&data).expect("unsupported JPEG");
    std::fs::write(&a[2], &out).unwrap();
    println!(
        "{} -> {} bytes ({:.1}%)",
        data.len(),
        out.len(),
        100.0 * out.len() as f64 / data.len() as f64
    );
}
