use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "silabs-data-gen")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Extract a Silabs CMSIS DFP .pack and emit a chip-database JSON.
    Chipdb {
        /// Path to the .pack file.
        #[arg(long)]
        pack: PathBuf,
        /// Path of the JSON output file (parent dirs created).
        #[arg(long)]
        out: PathBuf,
    },
    /// Parse an SVD file and dump peripherals (with fingerprints) as JSON.
    SvdDump {
        /// Path to a .svd file.
        #[arg(long)]
        svd: PathBuf,
    },
    /// Generate per-chip JSON files joining pdsc + SVD data.
    Gen {
        /// Path to the .pack file.
        #[arg(long)]
        pack: PathBuf,
        /// Output base directory (will create chips/ subdirectory).
        #[arg(long)]
        out_dir: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Chipdb { pack, out } => {
            let extract_dir = pack.with_extension("pack-extracted");
            silabs_data_gen::pack::extract_pack(&pack, &extract_dir)?;
            let pdsc_path = silabs_data_gen::pack::find_pdsc(&extract_dir)?;
            let xml = std::fs::read_to_string(&pdsc_path)?;
            let db = silabs_data_gen::pdsc::parse(&xml)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, serde_json::to_string_pretty(&db)?)?;
            eprintln!("wrote {} chips to {}", db.chips.len(), out.display());
            Ok(())
        }
        Cmd::SvdDump { svd } => {
            let xml = std::fs::read_to_string(&svd)?;
            let peripherals = silabs_data_gen::svd::parse(&xml)?;
            println!("{}", serde_json::to_string_pretty(&peripherals)?);
            Ok(())
        }
        Cmd::Gen { pack, out_dir } => {
            let extract_dir = pack.with_extension("pack-extracted");
            silabs_data_gen::pack::extract_pack(&pack, &extract_dir)?;
            let pdsc_path = silabs_data_gen::pack::find_pdsc(&extract_dir)?;
            let pdsc_xml = std::fs::read_to_string(&pdsc_path)?;
            let db = silabs_data_gen::pdsc::parse(&pdsc_xml)?;
            let perimap_entries = silabs_data_gen::perimap::compile()?;

            let chips_dir = out_dir.join("chips");
            std::fs::create_dir_all(&chips_dir)?;

            let total = db.chips.len();
            for chip in db.chips {
                let svd_path = extract_dir.join(&chip.svd);
                let svd_xml = std::fs::read_to_string(&svd_path)
                    .map_err(|e| anyhow::anyhow!("reading SVD {}: {e}", svd_path.display()))?;
                let peripherals = silabs_data_gen::svd::parse(&svd_xml)?;

                // The SVD's own `<interrupt>` blocks are intentionally not
                // consulted — they're an incomplete subset (radio
                // peripherals are missing). The per-chip CMSIS device
                // header sits next to the SVD in the pack tree. Convention:
                // `SVD/<FAMILY>/<CHIP>.svd` →
                // `Device/SiliconLabs/<FAMILY>/Include/<chip_lowercase>.h`.
                let hpath = match header_path_for_chip(&extract_dir, &chip) {
                    Some(hpath) if hpath.is_file() => hpath,
                    Some(hpath) => {
                        anyhow::bail!(
                            "no device header at {} for {} — header is the authoritative IRQ + series source and must be present",
                            hpath.display(),
                            chip.name,
                        );
                    }
                    None => {
                        anyhow::bail!("could not derive header path for {} (svd = {:?})", chip.name, chip.svd,);
                    }
                };
                let header_irqs = silabs_data_gen::header::parse_file(&hpath)
                    .map_err(|e| anyhow::anyhow!("reading header {}: {e}", hpath.display()))?;
                // Extract the `_SILICON_LABS_32B_SERIES_<N>_CONFIG_<M>`
                // identifier from the same header. Stored on `Chip` and
                // consumed downstream by `silabs-metapac-gen`'s
                // `build_chip_metadata_rs`.
                let series = silabs_data_gen::header::extract_series_file(&hpath)
                    .map_err(|e| anyhow::anyhow!("extracting series from {}: {e}", hpath.display()))?;
                let mut chip = chip;
                chip.series = Some(series);

                let chip_name = chip.name.clone();
                let chip_file = silabs_data_gen::chips::build(chip, &peripherals, &header_irqs, &perimap_entries);

                let out = chips_dir.join(format!("{chip_name}.json"));
                std::fs::write(&out, serde_json::to_string_pretty(&chip_file)?)?;
            }

            eprintln!("wrote {} chips to {}", total, chips_dir.display());
            Ok(())
        }
    }
}

/// Derive the per-chip CMSIS device header path from the SVD path inside an
/// extracted Silicon Labs CMSIS pack.
///
/// Convention (verified across `SiliconLabs.GeckoPlatform_EFR32MG24_DFP` and
/// `…_EFR32MG26_DFP` packs):
///
/// ```text
/// SVD path:    SVD/<FAMILY>/<CHIP>.svd
/// Header path: Device/SiliconLabs/<FAMILY>/Include/<chip_lowercase>.h
/// ```
///
/// Returns `None` if the SVD path doesn't match the expected layout.
fn header_path_for_chip(
    extract_dir: &std::path::Path,
    chip: &silabs_data_gen::pdsc::Chip,
) -> Option<std::path::PathBuf> {
    let svd = std::path::Path::new(&chip.svd);
    let parent = svd.parent()?;
    let family = parent.file_name()?.to_str()?;
    if parent.parent().and_then(|p| p.file_name())?.to_str()? != "SVD" {
        return None;
    }
    let header_file = format!("{}.h", chip.name.to_ascii_lowercase());
    Some(
        extract_dir
            .join("Device")
            .join("SiliconLabs")
            .join(family)
            .join("Include")
            .join(header_file),
    )
}
