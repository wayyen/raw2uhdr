//! dng2uhdr CLI.
//!
//! M1 scope: convert a single DNG to an Ultra HDR JPEG. The full flag surface
//! and batch mode (DESIGN.md §7) arrive in M2/M3.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use dng2uhdr_core::{
    DevelopOptions, Highlight, LibRawDeveloper, Pipeline, UltraHdrEncoder, WhiteBalance,
    DEFAULT_HDR_HEADROOM,
};

#[derive(Parser)]
#[command(name = "dng2uhdr", version, about = "Convert DNG raw files to Ultra HDR JPEGs")]
struct Cli {
    /// Input raw file (DNG).
    input: PathBuf,

    /// Output file (defaults to the input path with a .jpg extension).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// White balance.
    #[arg(long, value_enum, default_value_t = WbArg::Camera)]
    wb: WbArg,

    /// Highlight handling.
    #[arg(long, value_enum, default_value_t = HlArg::Clip)]
    highlight: HlArg,

    /// HDR headroom: linear multiplier between SDR white (1.0) and full scale.
    #[arg(long, default_value_t = DEFAULT_HDR_HEADROOM)]
    hdr_headroom: f32,

    /// Base-image JPEG quality (0-100).
    #[arg(long, default_value_t = 95)]
    quality: i32,

    /// Gain-map downscale factor (1 = full resolution).
    #[arg(long, default_value_t = 1)]
    gainmap_scale: i32,

    /// Overwrite the output if it already exists.
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let output = cli.output.clone().unwrap_or_else(|| cli.input.with_extension("jpg"));
    if output.exists() && !cli.overwrite {
        anyhow::bail!(
            "output {} already exists (use --overwrite)",
            output.display()
        );
    }

    let options = DevelopOptions {
        white_balance: match cli.wb {
            WbArg::Camera => WhiteBalance::Camera,
            WbArg::Auto => WhiteBalance::Auto,
        },
        highlight: match cli.highlight {
            HlArg::Clip => Highlight::Clip,
            HlArg::Blend => Highlight::Blend,
            HlArg::Rebuild => Highlight::Rebuild,
        },
        ..DevelopOptions::default()
    };

    let pipeline = Pipeline {
        developer: LibRawDeveloper::new(options),
        encoder: UltraHdrEncoder { quality: cli.quality, gainmap_scale: cli.gainmap_scale },
        hdr_headroom: cli.hdr_headroom,
    };

    let jpeg = pipeline
        .run(&cli.input)
        .with_context(|| format!("converting {}", cli.input.display()))?;

    std::fs::write(&output, &jpeg)
        .with_context(|| format!("writing {}", output.display()))?;

    println!("wrote {} ({} bytes)", output.display(), jpeg.len());
    Ok(())
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
                .unwrap_or_else(|_| format!("dng2uhdr={level},dng2uhdr_core={level}").into()),
        )
        .with_writer(std::io::stderr)
        .init();
}
