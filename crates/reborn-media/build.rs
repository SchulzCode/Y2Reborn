fn main() {
    let mut b = cc::Build::new();
    b.file("native/media.c").flag("-std=c11").warnings(true);
    for lib in [
        "libavformat",
        "libavcodec",
        "libavutil",
        "libswresample",
        "libswscale",
    ] {
        for p in pkg_config::Config::new()
            .probe(lib)
            .expect("FFmpeg development libraries")
            .include_paths
        {
            b.include(p);
        }
    }
    b.compile("reborn_media");
    println!("cargo:rerun-if-changed=native/media.c");
}
