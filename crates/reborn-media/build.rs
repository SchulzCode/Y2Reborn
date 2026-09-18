use std::{collections::BTreeSet, env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=native/media.c");

    // Keep the FFmpeg membrane out of the player executable. The shared
    // object is still the only implementation of decoding, filtering,
    // artwork conversion and resampling, but it is loaded on first media use
    // instead of by the ARM dynamic loader before Reborn::main.
    let names = [
        "libavformat",
        "libavcodec",
        "libavutil",
        "libavfilter",
        "libswresample",
        "libswscale",
    ];
    let mut include_paths = BTreeSet::new();
    let mut link_paths = BTreeSet::new();
    let mut link_files = BTreeSet::new();
    let mut libs = Vec::new();
    let mut seen_libs = BTreeSet::new();
    let mut ld_args = Vec::new();
    for name in names {
        let mut config = pkg_config::Config::new();
        let library = config
            .cargo_metadata(false)
            .probe(name)
            .unwrap_or_else(|e| panic!("FFmpeg development library {name}: {e}"));
        include_paths.extend(library.include_paths);
        link_paths.extend(library.link_paths);
        link_files.extend(library.link_files);
        for lib in library.libs {
            if seen_libs.insert(lib.clone()) {
                libs.push(lib);
            }
        }
        ld_args.extend(library.ld_args);
    }

    let mut build = cc::Build::new();
    build
        .file("native/media.c")
        .flag("-std=c11")
        .flag("-fPIC")
        .warnings(true)
        .cargo_metadata(false);
    for include in include_paths {
        build.include(include);
    }
    let objects = build
        .try_compile_intermediates()
        .expect("compile FFmpeg media membrane");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("libreborn_media.so");
    let mut command = build.get_compiler().to_command();
    command
        .arg("-shared")
        .arg("-Wl,-z,relro,-z,now")
        .arg("-o")
        .arg(&output)
        .args(objects);
    for path in link_paths {
        command.arg(format!("-L{}", path.display()));
    }
    for path in link_files {
        command.arg(path);
    }
    command.arg("-Wl,--no-as-needed");
    for lib in libs {
        command.arg(format!("-l{lib}"));
    }
    for args in ld_args {
        for arg in args {
            command.arg(arg);
        }
    }
    let status = command.status().expect("link FFmpeg media membrane");
    assert!(
        status.success(),
        "FFmpeg media membrane link failed with {status}"
    );
    println!(
        "cargo:rustc-env=REBORN_MEDIA_BUILD_LIB={}",
        output.display()
    );
}
