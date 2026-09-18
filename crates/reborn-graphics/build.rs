fn main() {
    let mut b = cc::Build::new();
    b.file("native/graphics.c").flag("-std=c11");
    for l in ["libdrm", "gbm", "egl", "glesv2"] {
        for p in pkg_config::Config::new()
            .probe(l)
            .expect("graphics development library")
            .include_paths
        {
            b.include(p);
        }
    }
    b.compile("reborn_graphics");
    println!("cargo:rerun-if-changed=native/graphics.c");
}
