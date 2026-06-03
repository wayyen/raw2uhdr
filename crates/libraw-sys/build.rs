//! Build script for `libraw-sys`.
//!
//! Provisioning priority (shared design with `uhdr-sys`):
//!   1. Explicit env override: `LIBRAW_LIB_DIR` (+ optional `LIBRAW_INCLUDE_DIR`).
//!   2. System library: when the `vendored` feature is OFF, or `DNG2UHDR_USE_SYSTEM=1`.
//!   3. Vendored static build (default): compile the pinned `third_party/LibRaw`
//!      C++ sources with the `cc` crate into a static `libraw.a`.
//!
//! LibRaw ships only a Makefile/autotools build, so the vendored path compiles
//! its source tree directly. We keep the build minimal (no RawSpeed/DNG SDK/GPL
//! demosaic packs); the conditional glue files compile to near-empty without
//! their feature macros defined.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let submodule = workspace_root.join("third_party/LibRaw");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LIBRAW_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LIBRAW_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=DNG2UHDR_USE_SYSTEM");

    let vendored_feature = env::var_os("CARGO_FEATURE_VENDORED").is_some();
    let force_system = env::var("DNG2UHDR_USE_SYSTEM").as_deref() == Ok("1");

    let mut include_dir = submodule.clone();

    if let Some(lib_dir) = env::var_os("LIBRAW_LIB_DIR") {
        // (1) Explicit override.
        let lib_dir = PathBuf::from(lib_dir);
        println!("cargo:warning=libraw-sys: linking explicit LIBRAW_LIB_DIR={}", lib_dir.display());
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!("cargo:rustc-link-lib=raw");
        if let Some(inc) = env::var_os("LIBRAW_INCLUDE_DIR") {
            include_dir = PathBuf::from(inc);
        }
        link_cxx();
    } else if !vendored_feature || force_system {
        // (2) System library.
        println!("cargo:warning=libraw-sys: using system LibRaw");
        match pkg_config::Config::new().probe("libraw") {
            Ok(lib) => {
                if let Some(p) = lib.include_paths.first() {
                    // System headers live under <prefix>/include/libraw, but the
                    // wrapper includes "libraw/libraw.h"; point bindgen at the
                    // parent so that include path resolves.
                    include_dir = p.parent().map(Path::to_path_buf).unwrap_or_else(|| p.clone());
                }
            }
            Err(e) => {
                println!("cargo:warning=libraw-sys: libraw not found via pkg-config ({e}); linking -lraw");
                println!("cargo:rustc-link-lib=raw");
                link_cxx();
            }
        }
    } else {
        // (3) Vendored static build.
        assert!(
            submodule.join("libraw/libraw.h").exists(),
            "third_party/LibRaw submodule is missing; run `git submodule update --init --recursive`"
        );
        println!("cargo:warning=libraw-sys: building vendored LibRaw (static) from {}", submodule.display());

        let mut sources = Vec::new();
        collect_cpp(&submodule.join("src"), &mut sources);
        sources.sort();
        assert!(!sources.is_empty(), "no LibRaw sources found under src/");

        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std("c++17")
            .include(&submodule)
            .warnings(false)
            // Silence LibRaw's noisy warnings under modern compilers.
            .flag_if_supported("-w")
            // Same transitive-include hardening as libultrahdr: ensure fixed-width
            // integer types are visible under newer libstdc++.
            .flag("-include")
            .flag("cstdint");
        for src in &sources {
            build.file(src);
        }
        // `cc` emits rustc-link-lib=static=raw and the search path automatically.
        build.compile("raw");

        include_dir = submodule.clone();
        link_cxx();
    }

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-x")
        .clang_arg("c++")
        .clang_arg("-std=c++17")
        .clang_arg(format!("-I{}", include_dir.display()))
        // Bind the C API surface and the public data structures only.
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_type("LibRaw_.*")
        .allowlist_var("LIBRAW_.*")
        // The C++ datastream classes and the LibRaw class carry vtables that
        // bindgen cannot lay out; we never touch them through the C API.
        .opaque_type("std::.*")
        .opaque_type("LibRaw")
        .opaque_type("LibRaw_.*_datastream")
        .opaque_type("LibRaw_abstract_datastream")
        // Bindgen mis-sizes opaque/forward-declared types (e.g. FILE/_IO_FILE),
        // producing layout asserts that fail to compile. We don't rely on them.
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate LibRaw bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings.write_to_file(&out_path).expect("failed to write bindings.rs");
}

/// Recursively collect `*.cpp` files under `dir` into `out`.
fn collect_cpp(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_cpp(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("cpp") {
            println!("cargo:rerun-if-changed={}", path.display());
            out.push(path);
        }
    }
}

/// LibRaw is C++; link the C++ runtime, pthread, and math libs.
fn link_cxx() {
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target.contains("msvc") {
        // MSVC links the C++ runtime automatically.
    } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=m");
}
