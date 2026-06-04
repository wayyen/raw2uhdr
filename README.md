# raw2uhdr

Convert DNG raw files from supported cameras into [Ultra HDR](https://developer.android.com/media/platform/hdr-image-format)
JPEGs with minimal loss of quality.

DNGs are developed through [LibRaw](https://www.libraw.org/) into a linear,
wide-gamut, high-bit-depth HDR rendition, which is then encoded to an Ultra HDR
JPEG via Google's [libultrahdr](https://github.com/google/libultrahdr). The
output is a standard JPEG (viewable anywhere) carrying an embedded gain map that
HDR-capable displays use to reconstruct the HDR rendition.

> **Status:** the core CLI is feature-complete — single-file and batch
> conversion, develop controls (white balance, exposure, color space,
> highlights), a content-aware HDR anchor, EXIF/orientation carry-over, and
> parallel directory processing all work end to end. Planned next: matching the
> camera JPEG look (libultrahdr API-1), a GUI preview, and more raw formats.
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
cd raw2uhdr
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

```bash
# Single file
raw2uhdr input.dng -o output.jpg

# Whole directory in parallel, into an output directory
raw2uhdr photos/ -o out/

# Recurse into subdirectories (mirrored under the output directory)
raw2uhdr photos/ -o out/ --recursive

# Multiple inputs; outputs land next to each source when -o is omitted
raw2uhdr a.dng b.dng c.dng
```

Available options:

```bash
raw2uhdr [OPTIONS] <INPUTS>...
  -o, --output <PATH>              # output file (single) or directory (batch)
      --suffix <S>                 # append to each output stem (before .jpg)
  -r, --recursive                  # recurse into input directories
  -j, --jobs <N>                   # parallel conversions (default: # CPUs)
      --wb <camera|auto>           # white balance
      --wb-mul <R> <G> <B> <G2>    # custom WB multipliers (overrides --wb)
      --exposure <EV>              # exposure compensation in stops (e.g. -1.0, +1.5)
      --color-space <srgb|p3|rec2020>  # output primaries (default: rec2020)
      --highlight <clip|blend|rebuild> # highlight handling (blend recovers more HDR)
      --hdr-headroom <auto|F>      # HDR headroom: content-aware (default) or fixed multiple
      --hdr-white-percentile <P>   # luminance percentile = SDR white in auto mode (default 99)
      --quality <0-100>            # base JPEG quality (default: 95)
      --gainmap-scale <N>          # gain-map downscale factor
      --no-exif                    # do not copy camera EXIF into the output
      --overwrite                  # overwrite existing outputs (else skipped)
  -v, --verbose                    # -v info, -vv debug
```

When scanning directories, `*.dng` files are picked up. In batch mode `-o` is an
output directory; existing outputs are skipped unless `--overwrite` is given.
The exit code is `0` on success, `1` if any file failed, `2` for usage errors.

Camera EXIF metadata (make, model, capture time, exposure, aperture, ISO,
focal length) is copied into the output by default; pass `--no-exif` to omit it.
Orientation is baked into the pixels during development, so the output is always
upright (EXIF orientation = 1).

Develop adjustments (WB, exposure, color space, highlights) change the rendition
but never affect HDR output — the gain map is always computed from whatever you
produce.

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
- `crates/core/tests/end_to_end.rs` — develops a real DNG and asserts the output
  is valid Ultra HDR with sane gain-map metadata and round-tripped EXIF. It is
  bring-your-own-DNG: set `DNG2UHDR_TEST_DNG=/path/to.dng` or drop a `*.dng` into
  `samples/` (auto-skips when none is present, so CI stays green).

## Project layout

```
crates/
  uhdr-sys/     FFI bindings to libultrahdr (bindgen) + vendored/system build
  uhdr/         safe RAII wrapper over uhdr-sys (encode + metadata probe)
  libraw-sys/   FFI bindings to LibRaw (bindgen) + vendored/system build
  libraw-dev/   safe LibRaw integration: develop raw -> linear RGB16 + EXIF
  core/         conversion pipeline: Developer/HdrEncoder traits, anchor, Pipeline
  cli/          the raw2uhdr binary
third_party/
  libultrahdr/  submodule, pinned to v1.4.0
  LibRaw/       submodule, pinned to 0.22.1
```

The pipeline is built around trait boundaries (`Developer`, `HdrEncoder`) so new
raw formats or output encoders can be added without touching the core. See
[`AGENTS.md`](AGENTS.md) for contributor guidance.

## License

Apache-2.0. Vendored dependencies retain their own licenses: libultrahdr
(Apache-2.0), LibRaw (LGPL-2.1 / CDDL-1.0), libjpeg-turbo (BSD/IJG).
