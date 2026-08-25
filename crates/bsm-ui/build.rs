// build.rs — generates a multi-resolution .ico from the BSM splash PNG and
// embeds it into the Windows .exe via winresource.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/sm512x512.png");

    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    use std::path::PathBuf;

    let png_bytes = include_bytes!("../../assets/sm512x512.png");
    let src = image::load_from_memory(png_bytes).expect("failed to decode sm512x512.png");

    let sizes: &[u32] = &[16, 32, 48, 64, 128, 256];
    let mut ico_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for &sz in sizes {
        let resized = src.resize_exact(sz, sz, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let entry = ico::IconImage::from_rgba_data(sz, sz, rgba.into_raw());
        ico_dir.add_entry(ico::IconDirEntry::encode(&entry).expect("ico encode failed"));
    }

    let out = std::env::var("OUT_DIR").unwrap();
    let ico_path = PathBuf::from(&out).join("bsm.ico");
    let f = std::fs::File::create(&ico_path).expect("create ico failed");
    ico_dir.write(f).expect("write ico failed");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.compile().expect("winresource compile failed");
}
