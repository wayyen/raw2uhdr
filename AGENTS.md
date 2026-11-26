# AGENTS.md

Guidance for AI agents and contributors working in this repository. Read this
alongside [`DESIGN.md`](DESIGN.md) (the authoritative design + roadmap) before
making changes.

## What this project is

A Rust workspace that converts DNG raw files into Ultra HDR JPEGs. The pipeline:

```
raw ─▶ LibRaw decode/WB/demosaic (camera RGB, 16-bit)
    ─▶ DNG camera profile (libraw-dev) ─▶ scene-linear ProPhoto f32
    ─▶ render (core): SDR base RGBA8 + HDR = SDR × highlight boost (half-float)
    ─▶ libultrahdr API-1 (gain map between them) ─▶ Ultra HDR JPEG
```

Core decisions (locked; do not silently change):
- **Rust** core linking LibRaw and libultrahdr via FFI.
- **API-1** (since the colour-grading work; was API-0): we render both the SDR
  base and the HDR rendition; libultrahdr computes the gain map. API-0's own
  tonemap gave a flat SDR base that never reached white (DESIGN.md OQ-5).
- **DNG camera profile fidelity**: DNGs are developed per the DNG SDK's
  semantics (ForwardMatrix interpolation, OpcodeList2 GainMap, HueSatMap,
  ProfileGainTableMap, BaselineExposure, LookTable, ProfileToneCurve with
  RGB-tone). Validate grading changes against camera RAW+JPEG pairs.
- **Renditions**: Display-P3 by default. SDR = 8-bit RGBA, sRGB transfer. HDR =
  64bpp RGBA half-float, linear, SDR white = `1.0`, nominal range
  `[0.0 .. 10000/203]`, and always a scalar multiple (≥1) of the SDR colour
  (single-channel gain map).

## Build & test

This host needs `LIBCLANG_PATH` for `bindgen` (libclang is under a versioned
LLVM path that clang-sys does not auto-detect):

```bash
export LIBCLANG_PATH=/usr/lib/llvm/23/lib64

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
| `crates/libraw-dev` | safe LibRaw integration + DNG profile rendering | done |
| `crates/core` | format-agnostic pipeline, traits, SDR/HDR renderer | done |
| `crates/cli` | the `raw2uhdr` binary | done |
| `third_party/libultrahdr` | submodule @ v2.0.2 | pinned |
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
- libultrahdr v2 defaults `UHDR_ENABLE_HEIF=ON` (git-clones libheif at
  configure time); we force it and SMPTE 2094-50 OFF to stay hermetic, and
  write both ISO 21496-1 and XMP gain-map metadata for viewer compatibility.
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
- Lens distortion correction (OpcodeList3 WarpRectilinear) is not applied.
- Raws without a DNG profile use a generic tone curve and +0.5 EV baseline;
  per-camera tuning would need reference pairs.

## Roadmap (see DESIGN.md §12)

M0 skeleton + FFI smoke test **(done)** → M1 develop path (one DNG end-to-end) →
M2 CLI → M3 batch/parallelism/quality knobs → M4 validation → M5 all LibRaw
formats → M6 colour grading (DNG profile rendering, API-1). Future: GUI preview
crate, OpcodeList3 distortion correction.
