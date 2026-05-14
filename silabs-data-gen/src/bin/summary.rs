use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

#[derive(Parser)]
#[command(name = "summary")]
#[command(about = "Summarize peripheral versions across EFR32 / EFM32 chips")]
struct Args {
    /// Optional peripheral kind filter (e.g. "GPIO"). Case-insensitive.
    #[arg(short = 'p', long = "peripheral")]
    peripheral: Option<String>,

    /// Directory containing per-chip JSON files (output of `silabs-data-gen gen`).
    #[arg(short = 'd', long = "chips-dir", default_value = "build/data/chips")]
    chips_dir: PathBuf,

    /// Directory containing curated register YAMLs. Used to detect peripherals
    /// routed to a (kind, version) without a corresponding YAML.
    #[arg(short = 'r', long = "registers-dir", default_value = "data/registers")]
    registers_dir: PathBuf,

    /// Path to families.toml. Used to derive the family axis from chip names.
    #[arg(
        short = 'f',
        long = "families",
        default_value = "../silabs-data-source/families.toml"
    )]
    families: PathBuf,
}

/// (kind_uppercase, family) -> (set of register_versions present, has_unsupported)
type Matrix = BTreeMap<String, BTreeMap<String, (BTreeSet<String>, bool)>>;

fn main() -> Result<()> {
    let args = Args::parse();

    let families =
        read_families(&args.families).with_context(|| format!("reading families from {}", args.families.display()))?;
    if families.is_empty() {
        bail!("no families found in {}", args.families.display());
    }

    let available_yamls = read_available_yamls(&args.registers_dir)
        .with_context(|| format!("reading registers dir {}", args.registers_dir.display()))?;

    let filter = args.peripheral.as_ref().map(|p| p.to_uppercase());

    let mut matrix: Matrix = BTreeMap::new();
    let mut chip_count = 0usize;
    let mut processed = 0usize;

    let entries =
        fs::read_dir(&args.chips_dir).with_context(|| format!("reading chips dir {}", args.chips_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        chip_count += 1;
        let content = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let chip: ChipFile = serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;

        let Some(family) = family_of(&chip.chip.name, &families) else {
            eprintln!("warning: no family match for chip {} (skipping)", chip.chip.name);
            continue;
        };
        processed += 1;

        for p in &chip.peripherals {
            let kind_up = p.kind.to_uppercase();
            if let Some(f) = &filter {
                if &kind_up != f {
                    continue;
                }
            }
            let entry = matrix
                .entry(kind_up)
                .or_default()
                .entry(family.clone())
                .or_insert_with(|| (BTreeSet::new(), false));
            let yaml_key = format!("{}_{}", p.kind, p.register_version);
            if available_yamls.contains(&yaml_key) {
                entry.0.insert(p.register_version.clone());
            } else {
                entry.1 = true;
            }
        }
    }

    eprintln!("processed {} of {} chip files", processed, chip_count);

    if matrix.is_empty() {
        if let Some(f) = &filter {
            eprintln!("no peripherals of kind '{}' found", f);
        } else {
            eprintln!("no peripherals found");
        }
        return Ok(());
    }

    print_table(&matrix, &families);
    print_sections(&matrix);

    Ok(())
}

fn read_families(path: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(path)?;
    let re = Regex::new(r#"(?m)^name\s*=\s*"([^"]+)"\s*$"#)?;
    let mut names: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    // Longest-first match to avoid prefix collisions (e.g. EFR32MG2 vs EFR32MG24).
    names.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    Ok(names)
}

fn read_available_yamls(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.insert(stem.to_string());
        }
    }
    Ok(out)
}

fn family_of(chip_name: &str, families: &[String]) -> Option<String> {
    families.iter().find(|f| chip_name.starts_with(f.as_str())).cloned()
}

fn family_label(family: &str) -> String {
    family
        .strip_prefix("EFR32")
        .or_else(|| family.strip_prefix("EFM32"))
        .unwrap_or(family)
        .to_string()
}

fn print_table(matrix: &Matrix, families: &[String]) {
    // Stable, human-friendly family order: alphabetical by label.
    let mut all_families: BTreeSet<&str> = BTreeSet::new();
    for kind_data in matrix.values() {
        for f in kind_data.keys() {
            all_families.insert(f.as_str());
        }
    }
    // Filter families list to the ones that actually appear.
    let cols: Vec<&str> = families
        .iter()
        .map(|s| s.as_str())
        .filter(|f| all_families.contains(f))
        .collect();
    // Sort columns alphabetically by label for deterministic output.
    let mut cols = cols;
    cols.sort_by(|a, b| family_label(a).cmp(&family_label(b)));

    println!("## Peripheral support by family\n");

    print!("| Peripheral |");
    for f in &cols {
        print!(" {} |", family_label(f));
    }
    println!();

    print!("|------------|");
    for _ in &cols {
        print!("--------|");
    }
    println!();

    for (kind, kind_data) in matrix {
        print!("| [{}](#{}) |", kind, kind.to_lowercase());
        for f in &cols {
            if let Some((versions, has_unsupported)) = kind_data.get(*f) {
                let mut parts: Vec<String> = versions.iter().cloned().collect();
                if *has_unsupported {
                    parts.push("❌".to_string());
                }
                print!(" {} |", parts.join(", "));
            } else {
                print!(" |");
            }
        }
        println!();
    }
    println!();
}

fn print_sections(matrix: &Matrix) {
    println!("## Detailed peripheral information\n");
    for (kind, kind_data) in matrix {
        println!("### {}\n", kind);

        // version -> families
        let mut by_version: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut unsupported: BTreeSet<String> = BTreeSet::new();
        for (family, (versions, has_unsupported)) in kind_data {
            for v in versions {
                by_version.entry(v.clone()).or_default().insert(family_label(family));
            }
            if *has_unsupported {
                unsupported.insert(family_label(family));
            }
        }

        if by_version.is_empty() && unsupported.is_empty() {
            println!("No supported or unsupported peripherals found.\n");
            continue;
        }

        println!("**Versions by family:**\n");
        for (v, fams) in &by_version {
            let list: Vec<&str> = fams.iter().map(|s| s.as_str()).collect();
            println!("- **{}**: {}", v, list.join(", "));
        }
        if !unsupported.is_empty() {
            let list: Vec<&str> = unsupported.iter().map(|s| s.as_str()).collect();
            println!("- **❌ Unsupported**: {}", list.join(", "));
        }
        println!();
    }
}
