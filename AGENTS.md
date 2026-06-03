# AGENTS.md

Guidance for AI agents and contributors working in this repository. Read this
alongside [`DESIGN.md`](DESIGN.md) (the authoritative design + roadmap) before
making changes.

## What this project is

A Rust workspace that converts DNG raw files into Ultra HDR JPEGs. The pipeline:

```
DNG ─▶ LibRaw develop (linear, Rec.2020, 16-bit) ─▶ RGBA half-float (BT.2100)
    ─▶ libultrahdr API-0 (derives SDR base + gain map) ─▶ Ultra HDR JPEG
```

Core decisions (locked; do not silently change):
- **Rust** core linking LibRaw and libultrahdr via FFI.
- **API-0**: hand libultrahdr the HDR rendition; it derives the SDR base and
  gain map. (The encoder trait is designed so API-1/3 can be added later.)
- **HDR rendition format**: 64bpp RGBA half-float, linear, BT.2100 gamut. SDR
  white maps to linear `1.0`; nominal half-float range is `[0.0 .. 10000/203]`.

## Build & test

This host needs `LIBCLANG_PATH` for `bindgen` (libclang is under a versioned
LLVM path that clang-sys does not auto-detect):

```bash
export LIBCLANG_PATH=/usr/lib/llvm/22/lib64

cargo build --workspace
cargo test  --workspace
cargo test  -p uhdr   --test smoke   # the M0 encode gate
cargo test  -p libraw-sys --test link
```

The first build is slow: it compiles vendored libultrahdr (+ its own
libjpeg-turbo) and all of LibRaw. Subsequent builds reuse the cached native
artifacts in `target/.../build/`.

Native-lib provisioning (both `*-sys` crates share the same logic in `build.rs`):
1. `*_LIB_DIR` / `*_INCLUDE_DIR` env override (highest priority),
2. system library when `--no-default-features` or `DNG2UHDR_USE_SYSTEM=1`,
3. vendored static build from `third_party/` submodules (default).

## Repository layout

| Crate | Role | Status |
|-------|------|--------|
| `crates/uhdr-sys` | bindgen FFI to libultrahdr; vendored cmake build | done (M0) |
| `crates/uhdr` | safe RAII wrapper over uhdr-sys | minimal (M0) |
| `crates/libraw-sys` | bindgen FFI to LibRaw C API; vendored cc build | done (M0) |
| `crates/libraw-dev` | safe LibRaw integration: develop raw → HDR | stub (M1) |
| `crates/core` | format-agnostic pipeline + traits | stub (M1+) |
| `crates/cli` | the `dng2uhdr` binary | placeholder (M2) |
| `third_party/libultrahdr` | submodule @ v1.4.0 | pinned |
| `third_party/LibRaw` | submodule @ 0.22.1 | pinned |

## Architecture & conventions

- **Modularity is a requirement**, not a nicety. The pipeline is organized
  around traits (`DESIGN.md` §6): `RawSource` → `Developer` → `HdrEncoder` →
  `OutputSink`. Adding a new input format = a new `RawSource`; a new output =
  a new `HdrEncoder`. Keep `core` format-agnostic; do not leak LibRaw or
  libultrahdr types into it.
- **`unsafe` stays in the `*-sys` crates and their thin safe wrappers** (`uhdr`,
  `libraw-dev`). `core` and above must be safe Rust.
- **Errors**: `thiserror` in libraries, `anyhow` at the CLI boundary. Per-file
  batch failures are collected, not fatal.
- **Logging**: `tracing`.
- Match the surrounding code's style; keep comments at the existing density and
  explain *why*, not *what*.

## Native build gotchas (already handled — keep them)

These fixes live in the `build.rs` files; don't regress them:
- `CMAKE_POLICY_VERSION_MINIMUM=3.5` (env passed to cmake) — libultrahdr pins
  libjpeg-turbo 3.0.1, whose `cmake_minimum_required` modern CMake rejects.
- `-include cstdint` forced for both libultrahdr and LibRaw — GCC 13+/16's
  leaner libstdc++ no longer transitively provides fixed-width integer types.
- libultrahdr is built with `BUILD_SHARED_LIBS=OFF` + `UHDR_BUILD_DEPS=ON`, so
  it builds libjpeg-turbo from source; we link the resulting static `libjpeg.a`.
  The combined `libuhdr.a` bundles `core` + `image_io` objects.
- bindgen on LibRaw uses `.layout_tests(false)` and opaque C++ datastream types
  (FILE / `LibRaw_*_datastream` cannot be laid out and aren't used via the C API).

## Working agreements

- **Do not bump submodule versions** without updating `DESIGN.md` and verifying
  the build; the libultrahdr API surface and LibRaw C API are version-sensitive.
- libultrahdr clones libjpeg-turbo into its own `third_party/` at build time;
  this is gitignored and the submodule is marked `ignore = untracked`. Don't
  commit it.
- Don't commit `samples/` or `*.dng` (gitignored) — keep the repo lean.
- Commit messages: imperative subject, explain non-obvious *why*. Branch before
  committing if on the default branch; commit/push only when asked.
- After a meaningful change, run the relevant test (`-p uhdr --test smoke` for
  encode-path changes) and report results honestly.

## Open questions to be resolved during implementation

Tracked in `DESIGN.md` §11. The high-impact ones:
- **OQ-1** normalization anchor (what linear value = SDR white) — biggest
  quality knob, will be exposed as `--hdr-white`.
- **OQ-2** highlight handling default (clip/blend/rebuild).
- **OQ-3** splicing EXIF without disturbing libultrahdr's MPF/XMP segments.

## Roadmap (see DESIGN.md §12)

M0 skeleton + FFI smoke test **(done)** → M1 develop path (one DNG end-to-end) →
M2 CLI → M3 batch/parallelism/quality knobs → M4 validation. Future: GUI preview
crate, more raw formats, API-1/3 encode paths.
