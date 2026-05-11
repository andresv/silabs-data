use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
                let svd_xml = std::fs::read_to_string(&svd_path).map_err(|e| {
                    anyhow::anyhow!("reading SVD {}: {e}", svd_path.display())
                })?;
                let (peripherals, interrupts) =
                    silabs_data_gen::svd::parse_all(&svd_xml)?;

                let chip_name = chip.name.clone();
                let chip_file = silabs_data_gen::chip_json::build(
                    chip,
                    &peripherals,
                    &interrupts,
                    &perimap_entries,
                );

                let out = chips_dir.join(format!("{chip_name}.json"));
                std::fs::write(&out, serde_json::to_string_pretty(&chip_file)?)?;
            }

            eprintln!("wrote {} chips to {}", total, chips_dir.display());
            Ok(())
        }
    }
}
