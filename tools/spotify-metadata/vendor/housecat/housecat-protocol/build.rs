use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read proto directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "proto") {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new("proto");
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();

    let descriptors = protox::compile(&files, [root]).expect("protox: compile vendored schemas");
    prost_build::Config::new()
        .include_file("_includes.rs")
        .compile_fds(descriptors)
        .expect("prost-build: generate Rust types");

    println!("cargo:rerun-if-changed=proto");
}
