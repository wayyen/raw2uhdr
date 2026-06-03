# dng2uhdr

Convert DNG raw files from supported cameras into [Ultra HDR](https://developer.android.com/media/platform/hdr-image-format)
JPEGs with minimal loss of quality.

DNGs are developed through [LibRaw](https://www.libraw.org/) into a linear,
wide-gamut, high-bit-depth HDR rendition, which is then encoded to an Ultra HDR
JPEG via Google's [libultrahdr](https://github.com/google/libultrahdr). The
output is a standard JPEG (viewable anywhere) carrying an embedded gain map that
HDR-capable displays use to reconstruct the HDR rendition.

> **Status:** early development. The native FFI build and encode path are
> working (M0); the end-to-end DNG → Ultra HDR CLI is being built out (M1+).
> See [`DESIGN.md`](DESIGN.md) for the full design and roadmap.

## How it works

```
DNG ──▶ LibRaw develop ──▶ linear Rec.2020 16-bit ──▶ RGBA half-float
    ──▶ libultrahdr (API-0: derive SDR + gain map) ──▶ Ultra HDR JPEG
```

The HDR rendition is handed to libultrahdr as 64bpp RGBA half-float, linear,
BT.2100 gamut; libultrahdr tone-maps the SDR base image and computes the gain
map. Keeping the pipeline linear and high-precision until the encoder is what
preserves quality. Design rationale lives in [`DESIGN.md`](DESIGN.md).

## Requirements

- **Rust** 1.95+ (2021 edition).
- **CMake** 3.31+ and a **C/C++ toolchain** (GCC or Clang) — to build the
  vendored libultrahdr.
- **libclang** — for `bindgen`. If it is not auto-detected, set `LIBCLANG_PATH`
  (e.g. `LIBCLANG_PATH=/usr/lib/llvm/22/lib64`).
- **nasm** — for libultrahdr's vendored libjpeg-turbo SIMD build.
- **git** — the native libraries are git submodules.

All native libraries (libultrahdr, LibRaw, and libultrahdr's libjpeg-turbo) are
**built from source by default**, so no system image libraries are required.

## Building

```bash
git clone --recurse-submodules <repo-url>
cd dng2uhdr
cargo build --workspace            # or: cargo build --release
```

If you cloned without `--recurse-submodules`:

```bash
git submodule update --init --recursive
```

If `bindgen` cannot find libclang:

```bash
LIBCLANG_PATH=/path/to/libclang/dir cargo build --workspace
```

### Using system libraries instead of vendored

The native libraries default to a vendored, statically linked build. To link
system-provided libraries instead:

```bash
# Per crate, via Cargo features:
cargo build --workspace --no-default-features

# Or force it at runtime for the whole build:
DNG2UHDR_USE_SYSTEM=1 cargo build --workspace
```

You can also point the build at specific prebuilt libraries:

| Variable | Effect |
|----------|--------|
| `UHDR_LIB_DIR` / `UHDR_INCLUDE_DIR` | Link a specific libultrahdr |
| `LIBRAW_LIB_DIR` / `LIBRAW_INCLUDE_DIR` | Link a specific LibRaw |
| `DNG2UHDR_USE_SYSTEM=1` | Prefer system libs over vendored |

## Usage

Single-file conversion works today:

```bash
dng2uhdr input.dng -o output.jpg
```

Available options:

```bash
dng2uhdr [OPTIONS] <INPUT>
  -o, --output <PATH>              # output (default: input with .jpg extension)
      --wb <camera|auto>           # white balance
      --wb-mul <R> <G> <B> <G2>    # custom WB multipliers (overrides --wb)
      --exposure <EV>              # exposure compensation in stops (e.g. -1.0, +1.5)
      --color-space <srgb|p3|rec2020>  # output primaries (default: rec2020)
      --highlight <clip|blend|rebuild> # highlight handling
      --hdr-headroom <F>           # HDR boost vs SDR white (default: 4.0)
      --quality <0-100>            # base JPEG quality (default: 95)
      --gainmap-scale <N>          # gain-map downscale factor
      --overwrite                  # overwrite an existing output
  -v, --verbose                    # -v info, -vv debug
```

Develop adjustments (WB, exposure, color space, highlights) change the rendition
but never affect HDR output — the gain map is always computed from whatever you
produce. Batch mode, recursive directories, parallelism, and EXIF/orientation
carry-over are planned for M2/M3 (see [`DESIGN.md`](DESIGN.md) §7).

> **Note on look:** development is intentionally neutral (linear, no camera tone
> curve), so the result will not match your camera's JPEG grading. `--exposure`
> and `--hdr-headroom` are the main brightness/HDR levers. Shaping the SDR look
> to resemble a finished camera JPEG (tone curve, contrast, saturation) needs the
> caller-supplied-SDR encode path (libultrahdr API-1), a planned enhancement.

## Testing

```bash
LIBCLANG_PATH=/path/to/libclang cargo test --workspace
```

Notable tests:

- `crates/uhdr/tests/smoke.rs` — encodes a synthetic half-float HDR gradient and
  verifies a valid Ultra HDR JPEG is produced.
- `crates/libraw-sys/tests/link.rs` — confirms LibRaw links and its C API works.

## Project layout

```
crates/
  uhdr-sys/     FFI bindings to libultrahdr (bindgen) + vendored/system build
  uhdr/         safe RAII wrapper over uhdr-sys
  libraw-sys/   FFI bindings to LibRaw (bindgen) + vendored/system build
  libraw-dev/   safe LibRaw integration: develop raw -> HDR rendition (M1)
  core/         format-agnostic conversion pipeline (traits) (M1+)
  cli/          the dng2uhdr binary (M2)
third_party/
  libultrahdr/  submodule, pinned to v1.4.0
  LibRaw/       submodule, pinned to 0.22.1
```

## License

Apache-2.0. Vendored dependencies retain their own licenses: libultrahdr
(Apache-2.0), LibRaw (LGPL-2.1 / CDDL-1.0), libjpeg-turbo (BSD/IJG).
