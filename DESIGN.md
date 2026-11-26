# raw2uhdr — Design Document

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

**Decision (revised 2026-11): we use API-1.** v1 used API-0 (libultrahdr derives the SDR base). Validation against camera RAW+JPEG pairs showed its built-in tonemap is unsuitable: for linear input it runs a Reinhard curve at a fixed ~49× headroom regardless of content, so SDR white lands at ~188/255 and the base looks dim and flat (OQ-5). We now render the SDR base and the HDR rendition ourselves (§5) and let libultrahdr compute the gain map between them.

### 1.3 DNG / raw development
A raw file is a container for raw sensor data plus metadata. Turning it into a viewable image ("developing") requires: black-level subtraction, white balance, demosaicing, color-space conversion via the camera color matrix, highlight handling, and a tone/gamma curve. We delegate this to **LibRaw** (pinned to 0.22.1), which has broad camera/format support and can emit **linear 16-bit Rec.2020** directly — exactly the input shape libultrahdr wants. Because LibRaw identifies files by content and the develop path is format-agnostic, raw2uhdr accepts **any raw format LibRaw decodes** (DNG, CR2/CR3, NEF, ARW, RAF, ORF, RW2, …), not just DNG. The set of extensions used to discover raws in a scanned directory lives in `core::format` (`SUPPORTED_EXTENSIONS`); explicitly-named files are passed to LibRaw regardless of extension.

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
- API-1/3/4 encode paths (interface allows; not wired in v1).
- Editing/grading controls beyond basic exposure.

### 2.3 Non-goals
- Being a full raw editor. We aim for a faithful, near-default development plus correct HDR encapsulation.

---

## 3. Decisions (confirmed with reviewer)

| Topic | Decision |
|-------|----------|
| Language / stack | **Rust** core, linking LibRaw and libultrahdr via FFI |
| Encode scenario | **API-1** — we render SDR + HDR, libultrahdr computes the gain map (was API-0; see §1.2) |
| Renditions | **Display-P3** (default): SDR RGBA8888 sRGB; HDR 64bpp RGBA half-float linear, a scalar multiple of the SDR |
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
│  Developer   │  (LibRaw decode/WB/demosaic + DNG camera profile → ProPhoto f32)
└──────┬───────┘
       │  linear Rec.2020 16-bit interleaved RGB
       ▼
┌──────────────┐   convert 16-bit linear → normalized RGBA half-float
│  HdrImage     │  + attach color-gamut / transfer / luminance scaling
│  builder      │
└──────┬───────┘
       │  uhdr_raw_image { fmt=64bppRGBAHalfFloat, cg=BT2100, ct=LINEAR }
       ▼
┌──────────────┐   render SDR + HDR (§5.2), API-1 encode
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

### 5.1 Development (LibRaw + DNG camera profile)
LibRaw decodes, white-balances and demosaics. It emits camera-native RGB: `output_color = 0`, linear, 16-bit, no auto-bright, `user_flip = 0`. We rotate at the end, because the profile's spatial maps are in sensor orientation. Colour is handled in `libraw-dev`. Without the Adobe DNG SDK, LibRaw ignores most of a DNG's rendering intent, and on Pixel DNGs that made output ~2 stops dark, vignetted and warm. So we implement the DNG SDK's semantics (`dng.rs` parses, `profile.rs` applies):
- **White balance**: from AsShotNeutral/AsShotWhiteXY via ColorMatrix interpolation. That means dual- or DNG 1.6 triple-illuminant weights from Robertson temperature/tint, passed to LibRaw as `user_mul`.
- **OpcodeList2 GainMap** (lens/colour shading, up to ~3.9× in Pixel corners): applied to the CFA before demosaic. LibRaw's `maximum` is raised so the brightened corners and WB don't clip in 16 bits; we then clip neutrally per pixel ourselves.
- **Camera → linear ProPhoto**: ForwardMatrix (or inverse ColorMatrix + Bradford). Then the HueSatMap, then the **ProfileGainTableMap**. That map is the camera's local tone mapping, applied pre-exposure with weights × 2^BaselineExposure and index `w·N` (SDK semantics). Then the **exposure ramp**: BaselineExposure (+offset) + `--exposure`, with the shadow black clip (`--black-clip`, default Adobe "Shadows 5", × ShadowScale). Then the **LookTable**.
- **Not clamped to [0,1]**: unlike the SDK's SDR path, values above 1 are kept as highlight data. We also record per-pixel scene brightness *before* the gain-table map, which compresses highlights into the SDR range; this drives the HDR lift (§5.2).
- **Non-DNG raws** (or `--no-profile`): LibRaw's camera matrix (`rgb_cam`) re-expressed in ProPhoto, with +0.5 EV baseline.
- Demosaic: default **AHD** (`user_qual = 3`). `--highlight blend/rebuild` still maps to LibRaw, except where shading gain maps are applied.

### 5.2 Rendering the two renditions (`core::render`)
- **SDR base.** The ProfileToneCurve is a natural cubic spline. Without one we use a generic camera-like curve. By default it is applied as a per-pixel luminance scale `T(Y)/Y` in ProPhoto (`--tone luminance`), which keeps scene colour ratios. Adobe's RGB-tone (`--tone adobe`, matches Lightroom/ACR) applies the curve's toe per channel and over-saturated warm hues by ~18% vs the camera JPEG; luminance tone matches per-hue chroma to ~5% (warm) / ~15% (sky blues). The result is then converted to the output gamut (default Display-P3) with soft gamut compression (ACES reference-gamut-compression form, limits from Rec.2020 primaries) instead of a hard clip. The result is sRGB-encoded to RGBA8888. White reaches 255.
- **HDR rendition.** It is the SDR linear colour × a scalar boost `2^(G·smoothstep(log2 0.09, log2 peak, log2 b))`:
  - `b` is the pre-local-tone-map scene brightness.
  - `peak` is its 99.9th percentile.
  - `G = log2(headroom)`.
  - Auto headroom = `2 × peak`, clamped to [2, 6]. That is one stop above where the SDR clips, plus whatever the camera recorded above it.
  - Shadows and midtones are therefore identical to the SDR, and only highlights lift. Hue and saturation match across renditions, so the gain map is single-channel. The boost range is pinned to `[1, headroom]` via `set_min_max_content_boost`.
- **Validation (Pixel 10 Pro XL RAW+JPEG pairs).**
  - SDR median and 5th-percentile L* are within ~1–2 of the camera JPEG, and near-neutral areas are within 0.3 Lab units.
  - HDR gain vs SDR level tracks the camera's gain map to within ~0.2 stops outdoors.
  - Headroom is 1.71/2.0 stops vs the camera's 1.74/2.54.
  - Remaining differences: no lens distortion correction (OpcodeList3), sky blues/cyans ~15% more saturated and ~3–5° more cyan, and the camera's scene-specific global HDR lift on some indoor shots.

### 5.3 What libultrahdr does (API-1)
Given both renditions, libultrahdr computes the gain map and muxes the final JPEG (base quality via `--quality`, `gainmap_scale_factor` via `--gainmap-scale`). `set_target_display_peak_brightness(headroom × 203)` makes `hdr_capacity_max` track the rendered headroom. It writes ISO 21496-1 + XMP gain-map metadata.

### 5.4 Metadata
- Orientation: applied during development (LibRaw `user_flip` honoring DNG orientation) so pixels are upright; we then write orientation=1.
- EXIF: copy a curated subset (camera, lens, exposure, date, GPS if present) from DNG into the output JPEG after encode. libultrahdr controls the APP segments it needs (MPF/XMP for the gain map); we splice EXIF without disturbing those. **OQ-3.**
- ICC: the base image is sRGB/Display-P3 per libultrahdr's SDR output; we tag accordingly rather than asserting Rec.2020 on an 8-bit base.

---

## 6. Module architecture

Cargo **workspace** (keeps the unsafe FFI isolated, the core pure, and leaves room for a GUI crate):

```
raw2uhdr/
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
raw2uhdr [OPTIONS] <INPUT>...

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
- **OQ-1 (normalization anchor):** **Resolved (M4).** The anchor (what maps to linear 1.0 = SDR white) is content-aware by default: SDR white is set at a luminance percentile (default 99th) of the developed image, so pixels above it become HDR highlights; the derived headroom = full-scale / anchor, capped (default 8×, 3 stops). Exposed as `--hdr-headroom auto|<number>` and `--hdr-white-percentile`. Critically, the headroom is also signalled to libultrahdr via `set_target_display_peak_brightness(headroom × 203 nits)` so the gain map's `hdr_capacity_max` matches the content — without this, a linear HDR intent leaves capacity at the 10000-nit (~49×) ceiling and displays under-apply the boost. Verified: `hdr_capacity_max` now tracks the resolved headroom across samples. (Final perceptual tuning of the percentile/cap is a user call on an HDR display.)
- **OQ-2 (highlight default):** **Resolved (M4).** Default is `clip` (matches LibRaw/dcraw, artifact-free, predictable). Measurement shows `blend`/`rebuild` recover roughly an extra stop of HDR headroom (clipping raises the percentile anchor), so `--highlight blend` is documented as the recommendation for scenes with bright skies/windows/speculars.
- **OQ-3 (EXIF splicing):** **Resolved (M2)** — we delegate to libultrahdr's own `uhdr_enc_set_exif_data`, which writes the APP1/EXIF segment in the correct position relative to its MPF/XMP/gain-map segments. We build a curated `Exif\0\0`+TIFF block from LibRaw-parsed metadata (make/model/software/datetime/exposure/fnumber/iso/focal, orientation=1). Verified round-tripping out of the output with an independent parser; no manual splicing needed.
- **OQ-4 (native lib provisioning):** **Resolved** — vendored, statically linked, built from pinned submodules by default; system libs are opt-in via the `vendored` feature switch / env overrides (see §6.2). libjpeg sub-decision (settled in M0): the default vendored build sets `UHDR_BUILD_DEPS=ON`, so libultrahdr builds its own libjpeg-turbo from source and we link the resulting static `libjpeg.a` — a fully hermetic build. (Reason: with `BUILD_SHARED_LIBS=OFF` libultrahdr forces a static-only library search, and this host has only a shared system libjpeg.)
- **OQ-5 (libultrahdr API-0 SDR quality):** **Resolved (2026-11) → API-1.** For linear input, API-0's Reinhard tonemap uses a fixed 10000/203 headroom, so SDR white sits at ~188/255 and the base is flat. We now render the SDR base ourselves (§5.2). OQ-1's percentile anchor was retired with it, because it tied midtone exposure to highlight content (grey ranged from 94 to 188/255). Exposure now comes from BaselineExposure + `--exposure`.
- **OQ-6 (LibRaw Rust binding):** confirm `libraw-rs` exposes the `dcraw_process` params we need at 0.22.1; otherwise local `-sys`.

---

## 12. Milestones
1. **M0 — Skeleton: DONE.** Workspace scaffolded (`uhdr-sys`, `uhdr`, `libraw-sys`, `libraw-dev`, `core`, `cli`); `third_party/` submodules pinned (libultrahdr v1.4.0, LibRaw 0.22.1); vendored static builds wired into `uhdr-sys` (cmake, `UHDR_BUILD_DEPS=ON`) and `libraw-sys` (cc), both with system/env override paths; FFI smoke test green (synthetic half-float HDR → valid Ultra HDR JPEG, verified via `is_uhdr_image`). Build notes: needed `CMAKE_POLICY_VERSION_MINIMUM=3.5` for libjpeg-turbo 3.0.1 under CMake 4.x, and a forced `-include cstdint` for both libs under GCC 16's leaner libstdc++.
2. **M1 — Develop path: DONE.** `libraw-dev` develops raw → linear Rec.2020 16-bit (LibRaw: linear gamma, `output_color=8`, 16-bit, no auto-bright, camera WB); `core` converts to RGBA half-float (headroom anchor, OQ-1, default 4.0) and encodes via the `Developer`/`HdrEncoder` traits + `Pipeline`; minimal CLI runs a single DNG end-to-end. Validated on a 12 MP Pixel DNG (4012×3020 → valid Ultra HDR, gain map present, ~1s). Full resolution preserved.
3. **M2 — CLI: DONE.** Flag surface (`--wb/--wb-mul/--exposure/--color-space/--highlight/--hdr-headroom/--quality/--gainmap-scale/--no-exif/--overwrite/-v`); typed errors with clean messages and exit codes (2 usage, 1 runtime, friendly missing-file check); EXIF carry-over via libultrahdr (OQ-3 resolved) with orientation baked upright (orientation tag = 1). Verified on Pixel DNGs: EXIF round-trips (make/model/exposure/iso/focal/datetime) and the gain map stays intact.
4. **M3 — Batch + parallelism: DONE.** Multiple inputs and directory inputs (`--recursive` mirrors subdirectories under the output dir); `-o` is a file (single) or output directory (batch); `--suffix`; parallel conversion with `rayon` (`-j` to cap threads, ~3.7× on 5 files); skip-on-exists unless `--overwrite`; per-file error collection with a run summary and exit codes (0 ok / 1 any failure / 2 usage). Quality knobs (`--quality`, `--gainmap-scale`, `--hdr-headroom`) landed earlier.
5. **M4 — Validation pass: DONE.** Added a decode-based gain-map metadata probe (`uhdr::probe`) and used it to drive decisions. Resolved OQ-1 (content-aware percentile anchor + `set_target_display_peak_brightness` so `hdr_capacity_max` tracks the content — confirmed across samples) and OQ-2 (clip default, blend recommended for highlights). Tests: deterministic auto-anchor unit test; end-to-end now asserts gain-map capacity is in `(1, cap]` and the gain map matches the base resolution. Remaining: perceptual tuning of the percentile/cap defaults on an HDR display (user-driven).
6. **M5 — All LibRaw formats: DONE.** Input broadened from DNG to every raw format LibRaw 0.22.1 decodes (CR2/CR3, NEF, ARW, RAF, ORF, RW2, X3F, …). Centralized the recognized-extension registry in `core::format` (`SUPPORTED_EXTENSIONS` + `is_supported_extension`), which drives directory scanning; explicitly-named files are passed to LibRaw regardless of extension. Documented the supported-format table in the README. No format-specific decode branches — LibRaw handles identification by content.
7. **M6 — Colour grading: DONE.** Validated against camera RAW+JPEG pairs.
   - DNG camera-profile rendering (§5.1): lens-shading gain maps, ForwardMatrix/triple-illuminant WB, HueSatMap, ProfileGainTableMap, BaselineExposure, LookTable, ProfileToneCurve.
   - API-1 encode with our own SDR base and a highlight-only HDR lift (§5.2), and Display-P3 output by default.
   - libultrahdr bumped v1.4.0 → v2.0.2: same C API; security/ISO 21496-1 fixes; libjpeg-turbo 3.1.0. HEIF and SMPTE 2094-50 are disabled to keep the build hermetic.
   - CLI: `--hdr-white-percentile` removed; added `--no-profile` and `--black-clip`; `--color-space` defaults to `p3`.
8. **Future:** GUI preview crate; OpcodeList3 distortion correction; per-camera tuning for non-DNG raws.
```
