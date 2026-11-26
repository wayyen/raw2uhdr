//! raw2uhdr CLI.
//!
//! Converts camera raw files (DNG, CR2/CR3, NEF, ARW, RAF, ORF, RW2, … any
//! format LibRaw supports) to Ultra HDR JPEGs. Supports single files, multiple
//! inputs, directories (optionally recursive), and parallel batch conversion.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use raw2uhdr_core::{
    is_supported_extension, ColorSpace, DevelopOptions, Headroom, Highlight, LibRawDeveloper,
    Pipeline, RenderOptions, ToneMode, UltraHdrEncoder, WhiteBalance, DEFAULT_MAX_HEADROOM,
};
use rayon::prelude::*;

#[derive(Parser)]
#[command(
    name = "raw2uhdr",
    version,
    about = "Convert camera raw files (DNG, CR2/CR3, NEF, ARW, and other LibRaw formats) to Ultra HDR JPEGs"
)]
struct Cli {
    /// Input raw files and/or directories.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output file (single input) or directory (batch). Default: alongside each input.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Suffix appended to each output file stem (before .jpg).
    #[arg(long, default_value = "")]
    suffix: String,

    /// Recurse into input directories.
    #[arg(short, long)]
    recursive: bool,

    /// Number of files to convert in parallel (default: number of CPUs).
    #[arg(short, long)]
    jobs: Option<usize>,

    /// White balance.
    #[arg(long, value_enum, default_value_t = WbArg::Camera)]
    wb: WbArg,

    /// Custom white-balance multipliers: R G B G2 (overrides --wb).
    #[arg(long, num_args = 4, value_names = ["R", "G", "B", "G2"])]
    wb_mul: Option<Vec<f32>>,

    /// Exposure compensation in stops (e.g. -1.0, +1.5), on top of the
    /// camera's baseline exposure. 0 = as the camera intended.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    exposure: f32,

    /// Shadow black clip in Adobe "Shadows" units (0 = none, default 5 ≈ 0.5%
    /// of raw white). Higher = deeper blacks.
    #[arg(long, default_value_t = raw2uhdr_core::DEFAULT_BLACK_CLIP)]
    black_clip: f32,

    /// How the tone curve treats colour: "luminance" keeps colour ratios
    /// (matches camera JPEGs); "adobe" is Adobe's RGB-tone, which adds
    /// saturation in shadows/midtones (matches Lightroom/ACR).
    #[arg(long, value_enum, default_value_t = ToneArg::Luminance)]
    tone: ToneArg,

    /// Ignore the DNG's embedded camera profile (look table, local tone
    /// mapping, tone curve, baseline exposure) and render neutrally.
    #[arg(long)]
    no_profile: bool,

    /// Output color space (primaries) of both the SDR base and HDR rendition.
    #[arg(long, value_enum, default_value_t = CsArg::P3)]
    color_space: CsArg,

    /// Highlight handling.
    #[arg(long, value_enum, default_value_t = HlArg::Clip)]
    highlight: HlArg,

    /// HDR headroom: peak HDR brightness as a multiple of SDR white. "auto"
    /// derives it from how far the scene's highlights extend above SDR white
    /// (2-6x); a number fixes it (e.g. 4.0 = 2 stops).
    #[arg(long, default_value = "auto")]
    hdr_headroom: String,

    /// Base-image JPEG quality (0-100).
    #[arg(long, default_value_t = 95)]
    quality: i32,

    /// Gain-map downscale factor (1 = full resolution).
    #[arg(long, default_value_t = 1)]
    gainmap_scale: i32,

    /// Do not copy camera EXIF metadata into the output.
    #[arg(long)]
    no_exif: bool,

    /// Overwrite outputs that already exist (otherwise they are skipped).
    #[arg(long)]
    overwrite: bool,

    /// Verbose logging (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Copy, Clone, ValueEnum)]
enum WbArg {
    Camera,
    Auto,
}

#[derive(Copy, Clone, ValueEnum)]
enum HlArg {
    Clip,
    Blend,
    Rebuild,
}

#[derive(Copy, Clone, ValueEnum)]
enum ToneArg {
    Luminance,
    Adobe,
}

#[derive(Copy, Clone, ValueEnum)]
enum CsArg {
    /// sRGB / BT.709
    Srgb,
    /// Display-P3
    P3,
    /// Rec.2020 / BT.2020
    Rec2020,
}

/// One unit of work: develop `input` and write the Ultra HDR JPEG to `output`.
struct Job {
    input: PathBuf,
    output: PathBuf,
}

/// Outcome of a single conversion.
enum Outcome {
    Wrote(usize),
    Skipped,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode> {
    let jobs = plan_jobs(cli)?;
    if jobs.is_empty() {
        bail!("no raw files found in the given inputs");
    }

    let pipeline = build_pipeline(cli)?;

    // Convert in parallel; order of results matches order of jobs.
    let convert = || -> Vec<(&Job, Result<Outcome>)> {
        jobs.par_iter()
            .map(|job| (job, convert_one(&pipeline, job, cli.overwrite)))
            .collect()
    };
    let results = match cli.jobs {
        Some(n) => rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build()
            .context("building thread pool")?
            .install(convert),
        None => convert(),
    };

    let (mut wrote, mut skipped, mut failed) = (0u32, 0u32, 0u32);
    for (job, result) in &results {
        match result {
            Ok(Outcome::Wrote(bytes)) => {
                wrote += 1;
                println!("wrote {} ({} bytes)", job.output.display(), bytes);
            }
            Ok(Outcome::Skipped) => {
                skipped += 1;
                println!("{}: skipped (output exists; use --overwrite)", job.output.display());
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e:#}", job.input.display());
            }
        }
    }

    if results.len() > 1 || skipped > 0 || failed > 0 {
        eprintln!("done: {wrote} converted, {skipped} skipped, {failed} failed");
    }

    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// Expand inputs into concrete (input -> output) jobs.
fn plan_jobs(cli: &Cli) -> Result<Vec<Job>> {
    // (file, base directory used to compute the relative output path)
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut have_dir = false;

    for input in &cli.inputs {
        if input.is_dir() {
            have_dir = true;
            let mut found = Vec::new();
            collect_raws(input, cli.recursive, &mut found);
            found.sort();
            for f in found {
                files.push((f, input.clone()));
            }
        } else if input.is_file() {
            let base = input.parent().unwrap_or(Path::new("")).to_path_buf();
            files.push((input.clone(), base));
        } else {
            bail!("input not found: {}", input.display());
        }
    }

    let batch = have_dir || files.len() > 1;

    // If -o points at (or names) a directory in batch mode, ensure it's usable.
    if batch {
        if let Some(out) = &cli.output {
            if out.is_file() {
                bail!("--output must be a directory when converting multiple files: {}", out.display());
            }
        }
    }

    let mut jobs = Vec::with_capacity(files.len());
    for (file, base) in files {
        let output = resolve_output(&file, &base, batch, cli)?;
        jobs.push(Job { input: file, output });
    }
    Ok(jobs)
}

/// Compute the output path for a single source file.
fn resolve_output(file: &Path, base: &Path, batch: bool, cli: &Cli) -> Result<PathBuf> {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let name = format!("{stem}{}.jpg", cli.suffix);

    match &cli.output {
        // No -o: write next to the source file.
        None => Ok(file.with_file_name(name)),

        Some(out) if batch => {
            // Treat -o as an output directory, preserving the input's path
            // relative to the scanned root (so subdirectories are mirrored).
            let rel_dir = file.strip_prefix(base).ok().and_then(|p| p.parent()).map(Path::to_path_buf);
            let mut dst = out.clone();
            if let Some(rel) = rel_dir {
                dst.push(rel);
            }
            dst.push(name);
            Ok(dst)
        }

        // Single input: -o is the output file, unless it's an existing directory.
        Some(out) if out.is_dir() => Ok(out.join(name)),
        Some(out) => Ok(out.clone()),
    }
}

/// Develop + encode one file, honoring the overwrite policy.
fn convert_one(
    pipeline: &Pipeline<LibRawDeveloper, UltraHdrEncoder>,
    job: &Job,
    overwrite: bool,
) -> Result<Outcome> {
    if job.output.exists() && !overwrite {
        return Ok(Outcome::Skipped);
    }
    if let Some(parent) = job.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }

    let jpeg = pipeline
        .run(&job.input)
        .with_context(|| format!("converting {}", job.input.display()))?;
    std::fs::write(&job.output, &jpeg)
        .with_context(|| format!("writing {}", job.output.display()))?;
    Ok(Outcome::Wrote(jpeg.len()))
}

fn build_pipeline(cli: &Cli) -> Result<Pipeline<LibRawDeveloper, UltraHdrEncoder>> {
    let white_balance = match &cli.wb_mul {
        Some(m) => WhiteBalance::Custom([m[0], m[1], m[2], m[3]]),
        None => match cli.wb {
            WbArg::Camera => WhiteBalance::Camera,
            WbArg::Auto => WhiteBalance::Auto,
        },
    };

    let options = DevelopOptions {
        white_balance,
        highlight: match cli.highlight {
            HlArg::Clip => Highlight::Clip,
            HlArg::Blend => Highlight::Blend,
            HlArg::Rebuild => Highlight::Rebuild,
        },
        exposure_ev: cli.exposure,
        use_profile: !cli.no_profile,
        black_clip: cli.black_clip.max(0.0),
        ..DevelopOptions::default()
    };

    let headroom = if cli.hdr_headroom.eq_ignore_ascii_case("auto") {
        Headroom::Auto { max: DEFAULT_MAX_HEADROOM }
    } else {
        let h: f32 = cli
            .hdr_headroom
            .parse()
            .with_context(|| format!("--hdr-headroom must be \"auto\" or a number, got {:?}", cli.hdr_headroom))?;
        Headroom::Fixed(h)
    };
    let color_space = match cli.color_space {
        CsArg::Srgb => ColorSpace::Srgb,
        CsArg::P3 => ColorSpace::DisplayP3,
        CsArg::Rec2020 => ColorSpace::Rec2020,
    };

    Ok(Pipeline {
        developer: LibRawDeveloper::new(options),
        render: RenderOptions {
            color_space,
            headroom,
            tone: match cli.tone {
                ToneArg::Luminance => ToneMode::Luminance,
                ToneArg::Adobe => ToneMode::Adobe,
            },
        },
        encoder: UltraHdrEncoder { quality: cli.quality, gainmap_scale: cli.gainmap_scale },
        keep_exif: !cli.no_exif,
    })
}

/// Recursively collect raw files under `dir`.
fn collect_raws(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_raws(&path, recursive, out);
            }
        } else if is_supported_extension(&path) {
            out.push(path);
        }
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("raw2uhdr={level},raw2uhdr_core={level}").into()),
        )
        .with_writer(std::io::stderr)
        .init();
}
