# dng2uhdr — Design Document

**Status:** Draft for review
**Date:** 2026-06-03
**Goal:** Convert DNG raw files from supported cameras into Ultra HDR JPEGs with minimal quality loss. CLI first, architected so a GUI preview layer can be added later, and so additional input/output formats can be plugged in without touching the core.

---

## 1. Background

### 1.1 Ultra HDR
Ultra HDR (Google's format, also called *JPEG_R* / gain-map JPEG) is a standard JPEG that embeds:
- a backward-compatible **SDR base image** (any legacy viewer shows this),
- a **gain map** (a secondary image describing per-pixel log-luminance boost),
- **metadata** (min/max content boost, gamma, offsets, HDR capacity) in an MPF/XMP container.

HDR-capable displays multiply the base image by the decoded gain map to reconstruct the HDR rendition; SDR displays ignore it. This is the format we emit.

### 1.2 libultrahdr
Google's reference C/C++ library for encoding/decoding Ultra HDR. Its encoder exposes several input "API scenarios":

| Scenario | Inputs | Who makes SDR | Who makes gain map |
|----------|--------|---------------|--------------------|
| API-0 | HDR raw only | libultrahdr (tonemaps) | libultrahdr |
| API-1 | HDR raw + SDR raw | caller | libultrahdr |
| API-3 | HDR raw + SDR JPEG | caller | libultrahdr |
| API-4 | SDR JPEG + gain map + metadata | caller | caller |

**Decision (confirmed): we use API-0.** We develop the DNG into a high-fidelity HDR rendition and let libultrahdr derive the SDR base and the gain map. This keeps the first version simple and leans on a well-tested tonemapper. The pipeline is structured so we can later switch to API-1/API-3 (caller-controlled SDR look) behind the same interface.

### 1.3 DNG / raw development
DNG is a container for raw sensor data plus metadata. Turning it into a viewable image ("developing") requires: black-level subtraction, white balance, demosaicing, color-space conversion via the camera color matrix, highlight handling, and a tone/gamma curve. We delegate this to **LibRaw** (system version 0.22.1 detected), which has broad camera/DNG support and can emit **linear 16-bit Rec.2020** directly — exactly the input shape libultrahdr wants.

---

## 2. Scope

### 2.1 In scope (v1, CLI)
- Single-file and batch DNG → Ultra HDR JPEG conversion.
- High-fidelity development path: linear, wide-gamut, high bit depth end-to-end.
- Sensible automatic defaults (white balance, exposure, highlight handling) with override flags.
- EXIF/orientation/ICC carry-over where meaningful.
- Quality/perf knobs (JPEG quality, gain-map downscale, demosaic algorithm).

### 2.2 Out of scope (v1) / future
- GUI preview (architecture must allow it; not built in v1).
- Non-DNG raw formats (CR3, NEF, ARW, …) — LibRaw supports them; we gate v1 on DNG but keep the decoder trait format-agnostic.
- API-1/3/4 encode paths (interface allows; not wired in v1).
- Editing/grading controls beyond basic exposure.

### 2.3 Non-goals
- Being a full raw editor. We aim for a faithful, near-default development plus correct HDR encapsulation.

---

## 3. Decisions (confirmed with reviewer)

| Topic | Decision |
|-------|----------|
| Language / stack | **Rust** core, linking LibRaw and libultrahdr via FFI |
| Encode scenario | **API-0** — develop HDR, libultrahdr derives SDR + gain map |
| HDR rendition format | **64bpp RGBA half-float, linear, BT.2100 (Rec.2020) gamut** |
| Native lib provisioning | **Vendored by default** — both libultrahdr and LibRaw built from pinned in-tree sources. A build-script switch (`vendored` cargo feature, on by default; plus env overrides) lets users opt into system libs instead. |

---

## 4. High-level pipeline

```
 DNG file
    │
    ▼
┌──────────────┐   raw bytes + metadata
│  Decoder     │  (LibRaw: open, unpack)
│  (RawSource) │
└──────┬───────┘
       │  CFA / sensor data + color matrices + WB + black/white levels
       ▼
┌──────────────┐   develop: demosaic, WB, highlight, Rec.2020, linear, 16-bit
│  Developer   │  (LibRaw dcraw_process, gamma=1, output_color=Rec2020, bps=16)
└──────┬───────┘
       │  linear Rec.2020 16-bit interleaved RGB
       ▼
┌──────────────┐   convert 16-bit linear → normalized RGBA half-float
│  HdrImage     │  + attach color-gamut / transfer / luminance scaling
│  builder      │
└──────┬───────┘
       │  uhdr_raw_image { fmt=64bppRGBAHalfFloat, cg=BT2100, ct=LINEAR }
       ▼
┌──────────────┐   API-0 encode
│  HdrEncoder  │  (libultrahdr: tonemap→SDR, build gain map, mux JPEG)
│  (UltraHdr)  │
└──────┬───────┘
       │  compressed Ultra HDR JPEG bytes
       ▼
┌──────────────┐   write file, splice EXIF/orientation
│  Sink        │
└──────────────┘
```

Each box is a trait boundary (Section 6) so formats/backends can be swapped.

---

## 5. Color & quality pipeline (the heart of "minimal quality loss")

The fidelity argument rests on keeping the data **linear, wide-gamut, and high-precision** until libultrahdr takes over, and never round-tripping through an 8-bit SDR JPEG on the way in (which is why API-0 with a half-float HDR input is the right entry point).

### 5.1 Development settings (LibRaw)
- `output_bps = 16` — 16-bit output, no premature truncation.
- `gamma = {1.0, 1.0}` — **linear** output (no tone curve baked in).
- `output_color = 8` — **Rec.2020 (BT.2020)** primaries, matching libultrahdr's BT.2100 gamut.
- `no_auto_bright = 1` by default — preserve true exposure; auto-bright is opt-in.
- `highlight` — default `0` (clip) vs `2` (blend) vs `5+` (rebuild); exposed as a flag. Highlight recovery matters a lot for HDR headroom; default chosen during validation (Section 11, OQ-2).
- `use_camera_wb = 1` by default (as-shot WB); `use_auto_wb` and explicit multipliers available.
- Demosaic: default **AHD** (`user_qual = 3`); DCB/others selectable.
- Output is interleaved RGB16.

### 5.2 16-bit linear → half-float RGBA
- LibRaw's 16-bit linear values are scene-linear up to its white point. We normalize to floating point so that **diffuse/SDR white maps to 1.0**, leaving values >1.0 as HDR headroom (specular highlights, recovered highlights).
- The normalization anchor (what counts as "1.0") directly controls how much HDR boost libultrahdr encodes. This is the single most important tuning parameter and is called out as **OQ-1** below.
- Alpha set to 1.0. Layout = `UHDR_IMG_FMT_64bppRGBAHalfFloat`, `cg = UHDR_CG_BT_2100`, `ct = UHDR_CT_LINEAR`.
- Conversion is done in Rust (chunked, parallelizable with `rayon`) — no intermediate disk image.

### 5.3 What libultrahdr does (API-0)
Given the linear half-float HDR image, libultrahdr tonemaps to an SDR base, computes the gain map (with configurable gain-map quality and downscale factor), and muxes the final JPEG. We pass through:
- base-image JPEG quality,
- gain-map quality and `gainmap_scale_factor`,
- `enable_gles` (GPU) off by default for deterministic CLI output,
- target output transfer (we request the standard gain-map output; HDR rendering left to the viewer).

### 5.4 Metadata
- Orientation: applied during development (LibRaw `user_flip` honoring DNG orientation) so pixels are upright; we then write orientation=1.
- EXIF: copy a curated subset (camera, lens, exposure, date, GPS if present) from DNG into the output JPEG after encode. libultrahdr controls the APP segments it needs (MPF/XMP for the gain map); we splice EXIF without disturbing those. **OQ-3.**
- ICC: the base image is sRGB/Display-P3 per libultrahdr's SDR output; we tag accordingly rather than asserting Rec.2020 on an 8-bit base.

---

## 6. Module architecture

Cargo **workspace** (keeps the unsafe FFI isolated, the core pure, and leaves room for a GUI crate):

```
dng2uhdr/
├─ Cargo.toml                  # workspace
├─ crates/
│  ├─ uhdr-sys/               # raw FFI bindings to libultrahdr C API (bindgen)
│  │   ├─ build.rs            # bindgen + link; locate/build libultrahdr
│  │   └─ src/lib.rs
│  ├─ uhdr/                   # safe Rust wrapper over uhdr-sys (RAII, Result)
│  ├─ libraw-dev/             # LibRaw integration (via libraw-rs or local sys binding)
│  ├─ core/                   # format-agnostic pipeline + traits  (no I/O of formats)
│  │   └─ src/
│  │       ├─ source.rs       # trait RawSource
│  │       ├─ develop.rs      # trait Developer
│  │       ├─ image.rs        # HdrImage, PixelLayout, color enums
│  │       ├─ encoder.rs      # trait HdrEncoder
│  │       ├─ sink.rs         # trait OutputSink + EXIF splice
│  │       └─ pipeline.rs     # orchestrates source→develop→encode→sink
│  ├─ cli/                    # binary: arg parsing (clap), batch, logging
│  └─ gui/   (future)         # eframe/Tauri preview; depends on core only
└─ DESIGN.md
```

### 6.1 Core traits (sketch)

```rust
/// A decoded-but-undeveloped raw image plus the metadata needed to develop it.
pub trait RawSource {
    fn probe(path: &Path) -> bool where Self: Sized;   // format sniff
    fn load(path: &Path) -> Result<Self> where Self: Sized;
    fn metadata(&self) -> &RawMetadata;                // WB, matrices, orientation, EXIF
}

/// Turns a RawSource into a linear, wide-gamut, high-bit-depth HDR image.
pub trait Developer {
    fn develop(&self, src: &dyn RawSource, opts: &DevelopOptions) -> Result<HdrImage>;
}

/// In-memory HDR rendition handed to the encoder.
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,        // RgbaF16
    pub gamut: ColorGamut,          // Bt2100
    pub transfer: TransferFn,       // Linear
    pub data: Vec<u8>,              // tightly packed
    pub white_scale: f32,           // normalization anchor (OQ-1)
}

/// Encodes an HdrImage into a final container.
pub trait HdrEncoder {
    fn encode(&self, img: &HdrImage, opts: &EncodeOptions) -> Result<Vec<u8>>;
}

pub trait OutputSink {
    fn write(&self, bytes: &[u8], meta: &RawMetadata, path: &Path) -> Result<()>;
}
```

The `Pipeline` selects a `RawSource` impl via `probe()` (registry pattern → future formats), runs `Developer`, then `HdrEncoder` (UltraHdr impl in v1), then `OutputSink`. Adding NEF/CR3 = another `RawSource`/dev config; adding AVIF gain-map output = another `HdrEncoder`. Nothing else changes.

### 6.2 FFI strategy
- **libultrahdr** has no Rust crate → we generate `uhdr-sys` with `bindgen` against `ultrahdr_api.h`.
- **LibRaw** (0.22.1 present on this host): we wrap it directly in a `libraw-sys` binding rather than depending on an external crate, so vendored/system selection is uniform across both libs. We only need `open_buffer/open_file`, `unpack`, `dcraw_process`, `dcraw_make_mem_image`, and metadata accessors.
- All `unsafe` confined to `*-sys` + the thin safe wrappers; `core` is safe Rust.

#### Build/link strategy (resolves OQ-4)
Both `uhdr-sys` and `libraw-sys` follow the same provisioning logic in their `build.rs`, in priority order:

1. **Explicit override (env):** if `UHDR_LIB_DIR` / `UHDR_INCLUDE_DIR` (resp. `LIBRAW_LIB_DIR` / `LIBRAW_INCLUDE_DIR`) are set, link against those paths. Highest priority, escape hatch for packagers.
2. **System library (opt-in):** with the `vendored` feature **disabled** (`--no-default-features`, or `DNG2UHDR_USE_SYSTEM=1`), discover via `pkg-config` (LibRaw exposes `libraw`; for libultrahdr fall back to a manual search since it's not on pkg-config here) and link the system copy.
3. **Vendored build (default):** with the `vendored` feature **enabled** (the default), build the library from a pinned in-tree source tree and **link it statically**, producing a self-contained binary with no runtime dependency on host copies.

Vendored sources live under `third_party/` as **git submodules** (pinned commits/tags), kept out of our own history:

```
third_party/
├─ libultrahdr/     # submodule, pinned tag; built via the `cmake` crate
└─ LibRaw/          # submodule, pinned tag; built via the `cc`/autotools wrapper
```

- libultrahdr builds cleanly with CMake → driven by the `cmake` build crate. We pin `UHDR_BUILD_DEPS` so its JPEG dependency is resolved deterministically (vendor libjpeg-turbo too, or require system libjpeg — decided in M0).
- LibRaw builds via its `cc`-compilable sources (no demosaic-pack/GPL extras needed for our use); the `cc` crate compiles the subset we link. If that proves fragile, fall back to invoking its autotools/CMake build.
- A top-level **`vendored` workspace feature** fans out to both sys crates, so `cargo build` (default) is fully self-contained and `cargo build --no-default-features` uses system libs. `build.rs` prints clear diagnostics stating which mode and which paths were used.

---

## 7. CLI design

```
dng2uhdr [OPTIONS] <INPUT>...

INPUT            One or more .dng files or directories (with --recursive)

Output:
  -o, --output <PATH>        Output file (single in) or directory (batch)
      --suffix <S>           Output name suffix (default: none; .jpg extension)
      --overwrite            Overwrite existing outputs

Development:
      --wb <MODE>            camera|auto|<r> <g> <b> <g2>   (default: camera)
      --exposure <EV>        Exposure compensation in stops (default: 0)
      --highlight <MODE>     clip|blend|rebuild             (default: chosen in OQ-2)
      --demosaic <ALG>       ahd|dcb|...                    (default: ahd)
      --no-auto-bright       (default on)  / --auto-bright

HDR / encode:
      --hdr-white <SCALE>    Normalization anchor for SDR white (OQ-1)
      --quality <0-100>      Base JPEG quality                (default: 95)
      --gainmap-quality <0-100>                              (default: 90)
      --gainmap-scale <N>    Gain-map downscale factor        (default: lib default)

General:
  -r, --recursive            Recurse into input directories
  -j, --jobs <N>             Parallel files (default: num CPUs)
      --keep-exif / --no-exif
  -v, --verbose  / -q, --quiet
      --dry-run
```

- `clap` for parsing. Batch mode parallelizes across files with `rayon`; per-file pixel conversion also parallel.
- Exit codes: 0 ok, 1 partial (some files failed), 2 usage error.

---

## 8. Error handling & logging
- `thiserror` for typed errors per stage (`DecodeError`, `DevelopError`, `EncodeError`, `IoError`); `anyhow` at the CLI boundary.
- Per-file failures in batch are collected and reported at the end; one bad file doesn't abort the run.
- `tracing` for structured logs; `-v` raises verbosity, `-q` silences all but errors.

---

## 9. Dependencies
| Dependency | Purpose | Notes |
|-----------|---------|-------|
| LibRaw | raw decode + develop | **vendored** (submodule) by default; 0.22.1 present for system mode |
| libultrahdr | Ultra HDR encode | **vendored** (submodule) by default; built statically |
| bindgen, cc, cmake (crate) | FFI build | build-time |
| pkg-config (crate) | system-lib discovery | build-time, system mode only |
| clap | CLI | |
| rayon | parallelism | |
| thiserror/anyhow | errors | |
| tracing | logging | |
| kamadak-exif or img-parts | EXIF read/splice | for metadata carry-over |

---

## 10. Testing & validation
- **Unit:** color conversion math (16-bit linear → f16, normalization), CLI parsing, format probing.
- **FFI smoke:** encode a synthetic gradient HdrImage → valid Ultra HDR JPEG; decode back with libultrahdr and check metadata present.
- **Golden:** small set of sample DNGs (various cameras) → outputs verified to (a) open as plain JPEG everywhere, (b) carry a valid gain map (validate with libultrahdr's `ultrahdr_app` / decoder), (c) round-trip EXIF orientation.
- **Quality checks:** compare HDR-decoded output luminance against the developed linear source; inspect for banding in the gain map, clipped highlights, gamut errors.
- **Perf:** wall-clock per 24–60MP file; batch throughput vs `-j`.

---

## 11. Open questions / things to validate during implementation
- **OQ-1 (normalization anchor):** What maps to linear 1.0 (SDR white)? Drives total HDR boost. Plan: derive from LibRaw white level / a configurable diffuse-white percentile; validate visually + against known-good Ultra HDR samples. Exposed as `--hdr-white`.
- **OQ-2 (highlight default):** clip vs blend vs rebuild — which default best preserves HDR headroom without artifacts? Decide via golden-image comparison.
- **OQ-3 (EXIF splicing):** **Resolved (M2)** — we delegate to libultrahdr's own `uhdr_enc_set_exif_data`, which writes the APP1/EXIF segment in the correct position relative to its MPF/XMP/gain-map segments. We build a curated `Exif\0\0`+TIFF block from LibRaw-parsed metadata (make/model/software/datetime/exposure/fnumber/iso/focal, orientation=1). Verified round-tripping out of the output with an independent parser; no manual splicing needed.
- **OQ-4 (native lib provisioning):** **Resolved** — vendored, statically linked, built from pinned submodules by default; system libs are opt-in via the `vendored` feature switch / env overrides (see §6.2). libjpeg sub-decision (settled in M0): the default vendored build sets `UHDR_BUILD_DEPS=ON`, so libultrahdr builds its own libjpeg-turbo from source and we link the resulting static `libjpeg.a` — a fully hermetic build. (Reason: with `BUILD_SHARED_LIBS=OFF` libultrahdr forces a static-only library search, and this host has only a shared system libjpeg.)
- **OQ-5 (libultrahdr API-0 SDR quality):** if the library's auto-tonemap SDR base looks weak, the trait design lets us graduate to API-1 (our own SDR rendition) — note as a likely v2 path.
- **OQ-6 (LibRaw Rust binding):** confirm `libraw-rs` exposes the `dcraw_process` params we need at 0.22.1; otherwise local `-sys`.

---

## 12. Milestones
1. **M0 — Skeleton: DONE.** Workspace scaffolded (`uhdr-sys`, `uhdr`, `libraw-sys`, `libraw-dev`, `core`, `cli`); `third_party/` submodules pinned (libultrahdr v1.4.0, LibRaw 0.22.1); vendored static builds wired into `uhdr-sys` (cmake, `UHDR_BUILD_DEPS=ON`) and `libraw-sys` (cc), both with system/env override paths; FFI smoke test green (synthetic half-float HDR → valid Ultra HDR JPEG, verified via `is_uhdr_image`). Build notes: needed `CMAKE_POLICY_VERSION_MINIMUM=3.5` for libjpeg-turbo 3.0.1 under CMake 4.x, and a forced `-include cstdint` for both libs under GCC 16's leaner libstdc++.
2. **M1 — Develop path: DONE.** `libraw-dev` develops raw → linear Rec.2020 16-bit (LibRaw: linear gamma, `output_color=8`, 16-bit, no auto-bright, camera WB); `core` converts to RGBA half-float (headroom anchor, OQ-1, default 4.0) and encodes via the `Developer`/`HdrEncoder` traits + `Pipeline`; minimal CLI runs a single DNG end-to-end. Validated on a 12 MP Pixel DNG (4012×3020 → valid Ultra HDR, gain map present, ~1s). Full resolution preserved.
3. **M2 — CLI: DONE.** Flag surface (`--wb/--wb-mul/--exposure/--color-space/--highlight/--hdr-headroom/--quality/--gainmap-scale/--no-exif/--overwrite/-v`); typed errors with clean messages and exit codes (2 usage, 1 runtime, friendly missing-file check); EXIF carry-over via libultrahdr (OQ-3 resolved) with orientation baked upright (orientation tag = 1). Verified on Pixel DNGs: EXIF round-trips (make/model/exposure/iso/focal/datetime) and the gain map stays intact.
4. **M3 — Batch + parallelism + quality knobs.**
5. **M4 — Validation pass:** resolve OQ-1/2/3, golden tests, docs.
6. **Future:** GUI preview crate; additional raw formats; API-1/3 encode paths.
```
