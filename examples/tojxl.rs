//! Recompress a JPEG as JPEG XL: `tojxl in.jpg out.jxl`. A third argument
//! `--smallest` leaves out the data that rebuilds the JPEG file.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let keep = a.get(3).is_none_or(|s| s != "--smallest");
    let options = smallpeg::JxlOptions::default().reconstruction(keep);
    let out = smallpeg::jpeg_to_jxl_with(&data, &options).expect("unsupported JPEG");
    std::fs::write(&a[2], &out.data).unwrap();
    println!(
        "{} -> {} bytes ({:.1}%), {}",
        data.len(),
        out.data.len(),
        100.0 * out.data.len() as f64 / data.len() as f64,
        if out.reconstruction {
            "the JPEG file can be rebuilt"
        } else {
            "image only"
        }
    );
}
