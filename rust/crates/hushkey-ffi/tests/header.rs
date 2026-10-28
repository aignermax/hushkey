//! include/hushkey.h is generated from the code; this keeps it current.
//! Regenerate with `HUSHKEY_BLESS=1 cargo test -p hushkey-ffi --test header`.

use std::path::Path;

#[test]
fn header_matches_the_code() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")).unwrap();
    let mut generated = Vec::new();
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("cbindgen parses the crate")
        .write(&mut generated);
    let generated = String::from_utf8(generated).unwrap();
    let header = crate_dir.join("../../include/hushkey.h");
    if std::env::var_os("HUSHKEY_BLESS").is_some() {
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::write(&header, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&header)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "include/hushkey.h is out of date: run HUSHKEY_BLESS=1 cargo test -p hushkey-ffi --test header"
    );
}
