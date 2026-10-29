//! On Windows, give hushkey.exe the logo as its icon (Explorer, Start Menu,
//! task manager) and version information.

fn main() {
    println!("cargo:rerun-if-changed=../../../assets/logo.png");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("hushkey.ico");
    write_ico(&ico);
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("FileDescription", "hushkey");
    res.set("ProductName", "hushkey");
    res.compile()
        .expect("embedding the icon (needs the Windows SDK's rc.exe)");
}

/// The 512 px logo at the sizes Windows asks for, made by averaging blocks.
fn write_ico(path: &std::path::Path) {
    let file = std::fs::File::open("../../../assets/logo.png").unwrap();
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "logo.png is RGBA");
    let size = info.width;
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for edge in [256u32, 128, 64, 48, 32, 16] {
        // 48 does not divide 512: average over the largest whole factor
        // (to 51 px), then pick the nearest pixels.
        let factor = (size / edge).max(1);
        let small = shrink(&buf, size, factor);
        let small_edge = size / factor;
        let image = if small_edge == edge {
            ico::IconImage::from_rgba_data(edge, edge, small)
        } else {
            ico::IconImage::from_rgba_data(edge, edge, nearest(&small, small_edge, edge))
        };
        dir.add_entry(ico::IconDirEntry::encode(&image).unwrap());
    }
    dir.write(std::fs::File::create(path).unwrap()).unwrap();
}

fn shrink(rgba: &[u8], size: u32, factor: u32) -> Vec<u8> {
    let out = size / factor;
    let mut result = Vec::with_capacity((out * out * 4) as usize);
    for oy in 0..out {
        for ox in 0..out {
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for y in oy * factor..(oy + 1) * factor {
                for x in ox * factor..(ox + 1) * factor {
                    let i = ((y * size + x) * 4) as usize;
                    let alpha = rgba[i + 3] as u64;
                    r += rgba[i] as u64 * alpha;
                    g += rgba[i + 1] as u64 * alpha;
                    b += rgba[i + 2] as u64 * alpha;
                    a += alpha;
                }
            }
            let n = (factor * factor) as u64;
            if a == 0 {
                result.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                result.extend_from_slice(&[
                    (r / a) as u8,
                    (g / a) as u8,
                    (b / a) as u8,
                    (a / n) as u8,
                ]);
            }
        }
    }
    result
}

/// Nearest-neighbour resize of a square image (48 px from 51 px).
fn nearest(rgba: &[u8], from: u32, to: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((to * to * 4) as usize);
    for y in 0..to {
        for x in 0..to {
            let (sx, sy) = (x * from / to, y * from / to);
            let i = ((sy * from + sx) * 4) as usize;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    out
}
