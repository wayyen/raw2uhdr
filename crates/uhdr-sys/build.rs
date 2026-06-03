//! Build script for `uhdr-sys`.
//!
//! Provisioning priority (shared design with `libraw-sys`):
//!   1. Explicit env override: `UHDR_LIB_DIR` (+ optional `UHDR_INCLUDE_DIR`).
//!   2. System library: when the `vendored` feature is OFF, or `DNG2UHDR_USE_SYSTEM=1`.
//!   3. Vendored static build (default): build libultrahdr from the pinned
//!      `third_party/libultrahdr` submodule and link it statically.
//!
//! The vendored archive (libuhdr.a) bundles libultrahdr's `core` and `image_io`
//! object files via the project's `combine_static_libs`. libultrahdr forces a
//! static-only library search when `BUILD_SHARED_LIBS=OFF`, so we build its
//! libjpeg-turbo dependency from source as well (`UHDR_BUILD_DEPS=ON`) and link
//! the resulting static libjpeg.a — yielding a fully self-contained build.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // crates/uhdr-sys -> crates -> <workspace root>
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let submodule = workspace_root.join("third_party/libultrahdr");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=UHDR_LIB_DIR");
    println!("cargo:rerun-if-env-changed=UHDR_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=DNG2UHDR_USE_SYSTEM");

    let vendored_feature = env::var_os("CARGO_FEATURE_VENDORED").is_some();
    let force_system = env::var("DNG2UHDR_USE_SYSTEM").as_deref() == Ok("1");

    // The include dir bindgen should parse. Defaults to the submodule header.
    let mut include_dir = submodule.clone();

    if let Some(lib_dir) = env::var_os("UHDR_LIB_DIR") {
        // (1) Explicit override.
        let lib_dir = PathBuf::from(lib_dir);
        println!("cargo:warning=uhdr-sys: linking explicit UHDR_LIB_DIR={}", lib_dir.display());
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!("cargo:rustc-link-lib=uhdr");
        if let Some(inc) = env::var_os("UHDR_INCLUDE_DIR") {
            include_dir = PathBuf::from(inc);
        }
        link_cxx_and_jpeg();
    } else if !vendored_feature || force_system {
        // (2) System library.
        println!("cargo:warning=uhdr-sys: using system libultrahdr");
        match pkg_config::Config::new().probe("libultrahdr") {
            Ok(lib) => {
                if let Some(p) = lib.include_paths.first() {
                    include_dir = p.clone();
                }
            }
            Err(_) => {
                // libultrahdr is frequently not registered with pkg-config; fall
                // back to a default link directive and rely on the linker path.
                println!("cargo:warning=uhdr-sys: libultrahdr not found via pkg-config; linking -luhdr from default paths");
                println!("cargo:rustc-link-lib=uhdr");
                link_cxx_and_jpeg();
            }
        }
    } else {
        // (3) Vendored static build (default).
        assert!(
            submodule.join("ultrahdr_api.h").exists(),
            "third_party/libultrahdr submodule is missing; run `git submodule update --init --recursive`"
        );
        println!("cargo:warning=uhdr-sys: building vendored libultrahdr (static) from {}", submodule.display());

        let dst = cmake::Config::new(&submodule)
            // libultrahdr pins libjpeg-turbo 3.0.1, whose CMakeLists declares a
            // pre-3.5 minimum that modern CMake rejects. This env var is honored
            // by CMake >= 3.31 and inherited by the turbojpeg ExternalProject.
            .env("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
            .define("BUILD_SHARED_LIBS", "OFF")
            // Build libjpeg-turbo from source so the static build is hermetic.
            .define("UHDR_BUILD_DEPS", "ON")
            .define("UHDR_ENABLE_INSTALL", "OFF")
            .define("UHDR_ENABLE_GLES", "OFF")
            .define("UHDR_ENABLE_INTRINSICS", "ON")
            .define("UHDR_BUILD_JAVA", "OFF")
            .define("CMAKE_BUILD_TYPE", "Release")
            // libultrahdr 1.4.0 uses `uint8_t` in headers without including
            // <cstdint>, relying on transitive includes that newer libstdc++
            // (GCC 13+) no longer provides. Force-include it for all TUs.
            .cxxflag("-include")
            .cxxflag("cstdint")
            .build_target("uhdr")
            .build();

        // build_target() leaves artifacts under <out>/build rather than installing.
        let build_dir = dst.join("build");
        println!("cargo:rustc-link-search=native={}", build_dir.display());
        println!("cargo:rustc-link-lib=static=uhdr");

        // libultrahdr builds its libjpeg-turbo dependency here (ExternalProject).
        let jpeg_dir = build_dir.join("turbojpeg/src/turbojpeg-build");
        println!("cargo:rustc-link-search=native={}", jpeg_dir.display());
        println!("cargo:rustc-link-lib=static=jpeg");

        include_dir = submodule.clone();
        link_cxx();
    }

    // Generate bindings against the resolved header location.
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        .allowlist_function("uhdr_.*")
        .allowlist_function("is_uhdr_image")
        .allowlist_type("uhdr_.*")
        .allowlist_var("UHDR_.*")
        .rustified_enum("uhdr_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate libultrahdr bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings.write_to_file(&out_path).expect("failed to write bindings.rs");
}

/// Link the C++ standard library and pthread that libultrahdr requires.
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
}

/// As [`link_cxx`], plus a JPEG codec for system/override builds where libuhdr
/// references libjpeg symbols that are not bundled into the archive.
fn link_cxx_and_jpeg() {
    link_cxx();
    if pkg_config::Config::new().probe("libjpeg").is_err() {
        println!("cargo:rustc-link-lib=dylib=jpeg");
    }
}
