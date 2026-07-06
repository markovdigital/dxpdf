use std::path::PathBuf;

use clap::Parser;
use dxpdf::{RenderOptions, DEFAULT_IMAGE_DPI};

#[derive(Parser)]
#[command(name = "dxpdf", about = "DOCX files to PDF converter")]
struct Cli {
    /// Input .docx file
    input: PathBuf,

    /// Output .pdf file (defaults to input path with .pdf extension)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Target resolution (pixels per inch) for embedded raster images.
    /// Higher values yield crisper images and larger PDFs.
    #[arg(long, default_value_t = DEFAULT_IMAGE_DPI)]
    image_dpi: f32,
}

fn main() -> Result<(), dxpdf::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let cli = Cli::parse();

    let output = cli
        .output
        .unwrap_or_else(|| cli.input.with_extension("pdf"));

    let options = RenderOptions::default().with_image_dpi(cli.image_dpi);

    let docx_bytes = std::fs::read(&cli.input)?;
    let pdf_bytes = dxpdf::convert_with_options(&docx_bytes, &options)?;
    std::fs::write(&output, &pdf_bytes)?;
    eprintln!("Converted {} -> {}", cli.input.display(), output.display());

    Ok(())
}
