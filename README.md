# raw2uhdr

Convert camera raw files into [Ultra HDR](https://developer.android.com/media/platform/hdr-image-format)
JPEGs with minimal loss of quality.

Raw files are developed through [LibRaw](https://www.libraw.org/) into a linear,
wide-gamut, high-bit-depth HDR rendition, which is then encoded to an Ultra HDR
JPEG via Google's [libultrahdr](https://github.com/google/libultrahdr). The
output is a standard JPEG (viewable anywhere) carrying an embedded gain map that
HDR-capable displays use to reconstruct the HDR rendition. Any raw format LibRaw
decodes is accepted — see [Supported input formats](#supported-input-formats).

> **Status:** the core CLI is feature-complete — single-file and batch
> conversion, develop controls (white balance, exposure, color space,
> highlights), EXIF/orientation carry-over, and parallel directory processing
> all work end to end, for any raw format LibRaw decodes. DNGs are rendered
> with their embedded camera profile, so output grading closely matches the
> camera's own JPEG. Planned next: a GUI preview. See [`DESIGN.md`](DESIGN.md) for the full design and roadmap.

## How it works

```
raw ──▶ LibRaw decode/WB/demosaic (camera RGB) ──▶ DNG profile (scene-linear ProPhoto)
    ──▶ render: SDR base (tone curve, P3, 8-bit) + HDR (SDR × highlight boost, half-float)
    ──▶ libultrahdr (API-1: gain map between the two) ──▶ Ultra HDR JPEG
```

For DNGs, the camera profile embedded in the file drives development:
ForwardMatrix/ColorMatrix interpolation for the shot's white point,
OpcodeList2 lens-shading gain maps, BaselineExposure, the ProfileGainTableMap
(the camera's local tone mapping), the LookTable, and the ProfileToneCurve.
LibRaw alone ignores all of these, which is why plain LibRaw output looks
flat, dark and vignetted next to the camera JPEG. Other raw formats use
LibRaw's camera matrix with a generic tone curve.

We render the SDR base ourselves (full contrast, white at 255) and an HDR
rendition that equals it in shadows and midtones and lifts only highlights,
by up to the HDR headroom. The headroom comes from how far the camera recorded
highlights above SDR white. libultrahdr then encodes the gain map between the
two. Design rationale lives in [`DESIGN.md`](DESIGN.md).

## Requirements

- **Rust** 1.95+ (2021 edition).
- **CMake** 3.31+ and a **C/C++ toolchain** (GCC or Clang) — to build the
  vendored libultrahdr.
- **libclang** — for `bindgen`. If it is not auto-detected, set `LIBCLANG_PATH`
  (e.g. `LIBCLANG_PATH=/usr/lib/llvm/23/lib64`).
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
      --exposure <EV>              # exposure compensation in stops, on top of the camera baseline
      --black-clip <N>             # shadow black clip, Adobe "Shadows" units (default 5; 0 = none)
      --tone <luminance|adobe>     # tone curve on colour: keep ratios (camera-JPEG-like, default)
                                   # or Adobe RGB-tone (Lightroom-like, more saturated)
      --no-profile                 # ignore the DNG camera profile (neutral rendering)
      --color-space <srgb|p3|rec2020>  # output primaries (default: p3)
      --highlight <clip|blend|rebuild> # highlight handling
      --hdr-headroom <auto|F>      # peak HDR brightness vs SDR white: auto (2-6x) or fixed
      --quality <0-100>            # base JPEG quality (default: 95)
      --gainmap-scale <N>          # gain-map downscale factor
      --no-exif                    # do not copy camera EXIF into the output
      --overwrite                  # overwrite existing outputs (else skipped)
  -v, --verbose                    # -v info, -vv debug
```

When scanning directories, files with known raw extensions are picked up (see
[Supported input formats](#supported-input-formats)); a file named explicitly on
the command line is always attempted, whatever its extension. In batch mode `-o`
is an output directory; existing outputs are skipped unless `--overwrite` is
given. The exit code is `0` on success, `1` if any file failed, `2` for usage
errors.

Camera EXIF metadata (make, model, capture time, exposure, aperture, ISO,
focal length) is copied into the output by default; pass `--no-exif` to omit it.
Orientation is baked into the pixels during development, so the output is always
upright (EXIF orientation = 1).

> **Note on look:** DNGs that carry a camera profile (e.g. Pixel phones) are
> rendered the way that profile intends. Validated against Pixel 10 Pro XL
> RAW+JPEG pairs, the SDR base matches the camera JPEG's brightness, contrast
> and shadow depth to within ~1-2 L* (median and 5th percentile), with neutral
> white balance and per-hue saturation within ~5% (sky blues ~15% richer). The HDR lift closely tracks the camera's gain map outdoors.
> Not reproduced: lens distortion correction (OpcodeList3 WarpRectilinear), so
> framing differs slightly, and the camera's own sharpening/noise reduction.
> Raws without a profile get LibRaw colour, +0.5 EV and a generic camera-like
> tone curve. For HDR-capable displays, `--hdr-headroom` controls how bright
> highlights go.

## Supported input formats

Decoding is handled by LibRaw, so raw2uhdr accepts any raw format the pinned
**LibRaw 0.22.1** decodes. LibRaw identifies files by their *content*, not their
extension — so a file named explicitly on the command line is always handed to
LibRaw regardless of extension. The extension list below is what raw2uhdr uses to
**discover raw files when scanning a directory**:

| Vendor | Extensions |
|--------|------------|
| Adobe / generic | `dng` |
| Canon | `cr2`, `cr3`, `crw` |
| Nikon | `nef`, `nrw` |
| Sony | `arw`, `srf`, `sr2` |
| Fujifilm | `raf` |
| Olympus / OM System | `orf` |
| Panasonic / Leica | `rw2`, `raw`, `rwl` |
| Pentax | `pef`, `ptx` |
| Samsung | `srw` |
| Sigma (Foveon) | `x3f` |
| Hasselblad | `3fr`, `fff` |
| Phase One | `iiq`, `cap`, `eip` |
| Mamiya / Leaf / Minolta | `mef`, `mos`, `mrw` |
| Epson | `erf` |
| Kodak | `dcr`, `kdc`, `k25`, `dcs`, `drf` |
| Other | `mdc`, `gpr` (GoPro), `bay` (Casio), `ari` (ARRI) |

The authoritative list of *cameras* LibRaw supports is published with each LibRaw
release; per-camera support is what ultimately determines whether a given file
develops. The registry lives in one place — `crates/core/src/format.rs`.

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
  libraw-dev/   safe LibRaw integration + DNG profile rendering -> scene-linear ProPhoto
  core/         pipeline: Developer/HdrEncoder traits, SDR/HDR renderer, Pipeline
  cli/          the raw2uhdr binary
third_party/
  libultrahdr/  submodule, pinned to v2.0.2
  LibRaw/       submodule, pinned to 0.22.1
```

The pipeline is built around trait boundaries (`Developer`, `HdrEncoder`) so new
raw formats or output encoders can be added without touching the core. See
[`AGENTS.md`](AGENTS.md) for contributor guidance.

## License

Apache-2.0. Vendored dependencies retain their own licenses: libultrahdr
(Apache-2.0 or MIT), LibRaw (LGPL-2.1 / CDDL-1.0), libjpeg-turbo (BSD/IJG).
