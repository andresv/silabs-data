//! Assemble a `silabs-metapac` crate from per-chip JSON + the curated
//! `data/registers/<kind>_<version>.yaml` IR snapshots.
//!
//! Two subcommands:
//!
//! - `gen`: Reads per-chip JSON + `data/registers/*.yaml` + `transforms/*.yaml`,
//!   renders the metapac crate into `--out-dir`. `data/registers/` is treated
//!   as committed input; this subcommand never writes there.
//!
//! - `seed`: Bootstrap-only. Extracts every peripheral on every chip directly
//!   from the SVDs, applies `transforms/<BLOCK>.yaml` if present, buckets by
//!   `(kind, register_version)` (assigned by perimap), and writes one
//!   `data/registers/<kind>_<version>.yaml` per bucket. Hash-bails when two
//!   chips' IRs land in the same bucket but disagree, surfacing the conflict
//!   for the human to resolve via transform / hand-curation / perimap split.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use silabs_data_gen::chips::ChipFile;
use silabs_data_gen::perimap;
use silabs_metapac_gen::codegen::{self, GenerateInput};
use silabs_metapac_gen::pac::{self, IpKey, module_name};
use silabs_metapac_gen::{crate_layout, extract, peripheral};
use svd_parser::ValidateLevel;

#[derive(Parser)]
#[command(name = "silabs-metapac-gen")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate the silabs-metapac crate from chip JSON + curated YAMLs.
    Gen {
        /// Per-chip JSON directory (output of `silabs-data-gen gen`).
        #[arg(long)]
        data_dir: PathBuf,
        /// Directory of committed `<kind>_<version>.yaml` IR snapshots.
        /// Read-only — this subcommand never writes to it.
        #[arg(long, default_value = "data/registers")]
        registers_yaml_dir: PathBuf,
        /// Output directory for the generated metapac crate.
        #[arg(long)]
        out_dir: PathBuf,
        /// Path(s) to .pack files for device.x rendering (needs raw SVDs).
        /// May be repeated.
        #[arg(long)]
        pack: Vec<PathBuf>,
        /// Only render device.x for these chip names. Other chips get a stub.
        /// May be repeated. If empty, every chip gets a real device.x.
        #[arg(long)]
        only: Vec<String>,
    },

    /// One-shot bootstrap: extract every peripheral from SVDs, bucket by
    /// `(kind, version)`, write `data/registers/<kind>_<version>.yaml` per
    /// bucket. Hash-bails on cross-chip divergence.
    Seed {
        /// Path to a .pack file. May be repeated.
        #[arg(long)]
        pack: Vec<PathBuf>,
        /// Per-chip JSON directory (output of `silabs-data-gen gen`).
        #[arg(long)]
        data_dir: PathBuf,
        /// Directory of chiptool transform YAMLs (root-level `transforms/`).
        #[arg(long, default_value = "transforms")]
        transforms_dir: PathBuf,
        /// Output directory for committed register IR snapshots.
        #[arg(long, default_value = "data/registers")]
        registers_yaml_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Gen {
            data_dir,
            registers_yaml_dir,
            out_dir,
            pack,
            only,
        } => run_gen(&data_dir, &registers_yaml_dir, &out_dir, &pack, &only),
        Cmd::Seed {
            pack,
            data_dir,
            transforms_dir,
            registers_yaml_dir,
        } => run_seed(&pack, &data_dir, &transforms_dir, &registers_yaml_dir),
    }
}

/// 8-char SHA-256 of an IR's canonical YAML serialisation. Used for
/// divergence detection during `seed`.
fn ir_hash(ir: &chiptool::ir::IR) -> String {
    let yaml = serde_yaml::to_string(ir).expect("serialise IR");
    let digest = Sha256::digest(yaml.as_bytes());
    let mut s = String::with_capacity(16);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in digest.iter().take(8) {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn load_chips(chips_dir: &Path) -> Result<Vec<ChipFile>> {
    if !chips_dir.is_dir() {
        return Err(anyhow!("expected per-chip JSON directory at {}", chips_dir.display()));
    }
    let mut chips: Vec<ChipFile> = Vec::new();
    for entry in std::fs::read_dir(chips_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let cf: ChipFile = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        chips.push(cf);
    }
    chips.sort_by(|a, b| a.chip.name.cmp(&b.chip.name));
    if chips.is_empty() {
        return Err(anyhow!("no chip JSON files found in {}", chips_dir.display()));
    }
    Ok(chips)
}

fn pack_extract_dirs(packs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::with_capacity(packs.len());
    for p in packs {
        let dir = p.with_extension("pack-extracted");
        if !dir.is_dir() {
            return Err(anyhow!(
                "pack not extracted at {}; run `silabs-data-gen chipdb --pack {}` first",
                dir.display(),
                p.display(),
            ));
        }
        dirs.push(dir);
    }
    Ok(dirs)
}

fn run_gen(
    data_dir: &Path,
    registers_yaml_dir: &Path,
    out_dir: &Path,
    packs: &[PathBuf],
    only: &[String],
) -> Result<()> {
    let chips_dir = data_dir.join("chips");
    let chips = load_chips(&chips_dir)?;
    eprintln!("Found {} chips in {}", chips.len(), chips_dir.display());

    let extract_dirs = if !packs.is_empty() {
        pack_extract_dirs(packs)?
    } else {
        Vec::new()
    };
    let only_set: BTreeSet<String> = only.iter().map(|s| s.to_ascii_lowercase()).collect();

    // ----- Discover register-banked peripheral kinds. -----
    //
    // Series 2 marks each peripheral with `#define <PERI>_HAS_SET_CLEAR`
    // in its per-peripheral CMSIS device header. We scan the extracted
    // pack(s) to recover the set rather than hard-coding it, so new
    // packs/families pick up new banked kinds automatically.
    let extract_refs: Vec<&Path> = extract_dirs.iter().map(PathBuf::as_path).collect();
    let banked_kinds: std::collections::HashSet<String> =
        silabs_metapac_gen::expand_aliases::discover_banked_kinds(&extract_refs)?;
    if extract_dirs.is_empty() {
        eprintln!(
            "warning: no --pack passed; cannot discover banked peripherals — \
             generated metapac will lack SET/CLR/TGL register aliases",
        );
    } else {
        eprintln!(
            "Discovered {} banked peripheral kind(s) from {} pack(s)",
            banked_kinds.len(),
            extract_dirs.len(),
        );
    }

    // ----- Collect every (kind, version) referenced by any chip. -----
    let mut module_users: BTreeMap<IpKey, BTreeSet<String>> = BTreeMap::new();
    for chip in &chips {
        let feat = crate_layout::feature_name(&chip.chip.name);
        for p in &chip.peripherals {
            if p.name.ends_with("_S") && !p.name.ends_with("_NS") {
                continue;
            }
            let key: IpKey = (p.kind.clone(), p.register_version.clone());
            module_users.entry(key).or_default().insert(feat.clone());
        }
    }

    // ----- Load `data/registers/<kind>_<version>.yaml` for each key. -----
    let mut irs: BTreeMap<IpKey, chiptool::ir::IR> = BTreeMap::new();
    for key in module_users.keys() {
        let mod_name = module_name(&key.0, &key.1);
        let path = registers_yaml_dir.join(format!("{mod_name}.yaml"));
        if !path.is_file() {
            bail!(
                "no register YAML for `{mod_name}` at {} — run `./d seed` to bootstrap, or hand-curate the file.",
                path.display()
            );
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let mut ir: chiptool::ir::IR =
            serde_yaml::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        // For Series 2 banked peripherals, materialise the
        // _SET/_CLR/_TGL alias views at +0x1000/+0x2000/+0x3000. The
        // SVD/YAML only carry the base layout; the per-peripheral CMSIS
        // device header is what marks these peripherals with
        // `_HAS_SET_CLEAR` (see `discover_banked_kinds` above).
        if banked_kinds.contains(&key.0) {
            silabs_metapac_gen::expand_aliases::expand_series2_aliases(&mut ir);
        }
        irs.insert(key.clone(), ir);
    }

    // ----- Emit src/peripherals/<kind>_<version>.rs + IR metadata -----
    std::fs::create_dir_all(out_dir.join("src/chips"))
        .with_context(|| format!("create out dir {}", out_dir.display()))?;
    pac::write_peripherals_dir(&irs, &out_dir.join("src/peripherals"))?;
    pac::write_common_module(&out_dir.join("src/common.rs"))?;

    // IR-metadata module (`metadata.rs` type defs) + per-kind static IR
    // (`src/registers/<kind>_<version>.rs`). Mirrors stm32-metapac's
    // layout — see `silabs_metapac_gen::ir_metadata`.
    silabs_metapac_gen::ir_metadata::write_metadata_module(out_dir)?;
    silabs_metapac_gen::ir_metadata::write_registers_dir(&irs, &out_dir.join("src/registers"))?;

    // ----- Cargo.toml + lib.rs -----
    let chip_features: Vec<String> = chips.iter().map(|c| crate_layout::feature_name(&c.chip.name)).collect();
    crate_layout::write_cargo_toml(&chip_features, &out_dir.join("Cargo.toml"))?;
    crate_layout::write_build_rs(&out_dir.join("build.rs"))?;

    let peripheral_modules: Vec<(String, Vec<String>)> = module_users
        .iter()
        .map(|(key, users)| {
            let mod_name = module_name(&key.0, &key.1);
            let mut users: Vec<String> = users.iter().cloned().collect();
            users.sort();
            (mod_name, users)
        })
        .collect();
    crate_layout::write_lib_rs(&chip_features, &peripheral_modules, &out_dir.join("src/lib.rs"))?;

    std::fs::write(
        out_dir.join("README.md"),
        "# silabs-metapac\n\n\
         Generated Silicon Labs PAC. Do not edit by hand — regenerate via `silabs-metapac-gen`.\n",
    )?;

    // ----- Per-chip mod.rs + device.x -----
    for chip in &chips {
        let feat = crate_layout::feature_name(&chip.chip.name);
        let chip_dir = out_dir.join("src/chips").join(&feat);
        std::fs::create_dir_all(&chip_dir)?;

        let mod_rs = crate_layout::build_chip_mod_rs(chip);
        std::fs::write(chip_dir.join("mod.rs"), mod_rs)?;

        // Iterable chip metadata sibling — consumed by HAL build scripts
        // (e.g. embassy-silabs/build.rs) to generate singleton lists.
        let metadata_rs = crate_layout::build_chip_metadata_rs(chip);
        std::fs::write(chip_dir.join("metadata.rs"), metadata_rs)?;

        let device_x_path = chip_dir.join("device.x");
        let render_device_x = !extract_dirs.is_empty() && (only_set.is_empty() || only_set.contains(&feat));
        if render_device_x {
            let svd_path = extract_dirs
                .iter()
                .map(|d| d.join(&chip.chip.svd))
                .find(|p| p.is_file())
                .ok_or_else(|| anyhow!("SVD {} missing for {}", chip.chip.svd, chip.chip.name))?;
            // Forward the chip JSON's interrupt list (header-derived) as
            // the authoritative IRQ table — chiptool's SVD-derived list
            // is discarded inside `codegen::generate`.
            let irqs: Vec<codegen::Interrupt<'_>> = chip
                .interrupts
                .iter()
                .map(|i| codegen::Interrupt {
                    name: i.name.as_str(),
                    value: i.value,
                    description: i.description.as_deref(),
                })
                .collect();
            let generated = codegen::generate(GenerateInput {
                svd_path: &svd_path,
                transforms: &[],
                interrupts: &irqs,
            })
            .with_context(|| format!("device.x codegen {}", chip.chip.name))?;
            std::fs::write(&device_x_path, &generated.device_x)?;
        } else if !device_x_path.exists() {
            std::fs::write(&device_x_path, crate_layout::stub_device_x(&chip.chip.name))?;
        }
    }

    eprintln!("Wrote silabs-metapac crate to {}", out_dir.display());
    Ok(())
}

/// Hash-bail divergence report.
fn bail_divergence(
    key: &IpKey,
    a_chip: &str,
    a_peripheral: &str,
    a_hash: &str,
    b_chip: &str,
    b_peripheral: &str,
    b_hash: &str,
) -> ! {
    eprintln!();
    eprintln!("=== seed divergence: (kind={}, version={}) ===", key.0, key.1);
    eprintln!("  {a_chip} :: {a_peripheral}  →  IR hash {a_hash}");
    eprintln!("  {b_chip} :: {b_peripheral}  →  IR hash {b_hash}");
    eprintln!();
    eprintln!("Both peripherals route to the same (kind, version) but extract to");
    eprintln!("different IRs. Resolve via one of:");
    eprintln!("  1. write a transforms/<BLOCK>.yaml rule that normalises both");
    eprintln!("     extractions to one canonical shape;");
    eprintln!(
        "  2. hand-curate data/registers/{}.yaml with the canonical/superset",
        module_name(&key.0, &key.1)
    );
    eprintln!("     shape;");
    eprintln!("  3. add a perimap entry in silabs-data-gen/src/perimap.rs that");
    eprintln!("     splits one of these peripherals to a distinct version label.");
    std::process::exit(1);
}

fn run_seed(packs: &[PathBuf], data_dir: &Path, transforms_dir: &Path, registers_yaml_dir: &Path) -> Result<()> {
    if packs.is_empty() {
        bail!("at least one --pack is required for seed");
    }
    let extract_dirs = pack_extract_dirs(packs)?;
    let chips = load_chips(&data_dir.join("chips"))?;
    eprintln!("Seeding {} chips across {} packs", chips.len(), packs.len());

    let cfg = svd_parser::Config::default()
        .expand_properties(true)
        .validate_level(ValidateLevel::Disabled);

    // (kind, version) → (block, ir, hash, first-claiming chip, first-claiming peripheral).
    struct Bucket {
        block: String,
        ir: chiptool::ir::IR,
        hash: String,
        chip: String,
        peripheral: String,
    }
    let mut buckets: BTreeMap<IpKey, Bucket> = BTreeMap::new();

    for chip in &chips {
        let svd_path = extract_dirs
            .iter()
            .map(|d| d.join(&chip.chip.svd))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "SVD {} not found in any --pack extract dir for chip {}",
                    chip.chip.svd,
                    chip.chip.name
                )
            })?;
        let raw = std::fs::read_to_string(&svd_path).with_context(|| format!("read SVD {}", svd_path.display()))?;
        let preprocessed = peripheral::strip_secure_peripherals(&raw)
            .with_context(|| format!("strip _S in {}", svd_path.display()))?;
        let device = svd_parser::parse_with_config(&preprocessed, &cfg)
            .with_context(|| format!("parse SVD {}", svd_path.display()))?;

        // Build a per-chip name → routed-record lookup from chip JSON.
        let mut by_name: BTreeMap<&str, &silabs_data_gen::chips::PeripheralInstance> = BTreeMap::new();
        for p in &chip.peripherals {
            by_name.insert(p.name.as_str(), p);
        }

        for periph in &device.peripherals {
            let pname = periph.name.as_str();
            let inst = match by_name.get(pname) {
                Some(p) => *p,
                None => bail!(
                    "chip JSON for {} lacks peripheral `{pname}` (chips out of sync with SVD)",
                    chip.chip.name
                ),
            };
            let key: IpKey = (inst.kind.clone(), inst.register_version.clone());
            let ir = extract::extract_ip(periph, &inst.block, &inst.register_version, transforms_dir).with_context(
                || {
                    format!(
                        "extract {pname} (kind={}, version={}) from {}",
                        inst.kind, inst.register_version, chip.chip.name
                    )
                },
            )?;
            let hash = ir_hash(&ir);

            match buckets.get(&key) {
                Some(existing) => {
                    if existing.hash != hash {
                        bail_divergence(
                            &key,
                            &existing.chip,
                            &existing.peripheral,
                            &existing.hash,
                            &chip.chip.name,
                            pname,
                            &hash,
                        );
                    }
                }
                None => {
                    buckets.insert(
                        key,
                        Bucket {
                            block: inst.block.clone(),
                            ir,
                            hash,
                            chip: chip.chip.name.clone(),
                            peripheral: pname.to_owned(),
                        },
                    );
                }
            }
        }
    }

    std::fs::create_dir_all(registers_yaml_dir).with_context(|| format!("create {}", registers_yaml_dir.display()))?;
    for (key, bucket) in &buckets {
        let fname = format!("{}.yaml", module_name(&key.0, &key.1));
        let path = registers_yaml_dir.join(&fname);
        let mut f = std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        serde_yaml::to_writer(&mut f, &bucket.ir).with_context(|| format!("serialise IR to {}", path.display()))?;
    }
    // Mention the block name for traceability — useful when debugging
    // mismatches between chip JSON and committed YAML.
    let _ = perimap::compile()?; // sanity check: perimap entries still compile.
    eprintln!(
        "Wrote {} register YAMLs to {}",
        buckets.len(),
        registers_yaml_dir.display()
    );
    // Surface bucket→block→first-claim summary so future hand-tweakers see
    // which chip each YAML was first extracted from.
    for (key, bucket) in &buckets {
        eprintln!(
            "  {}_{}.yaml  block={}  seeded from {} :: {}",
            key.0, key.1, bucket.block, bucket.chip, bucket.peripheral
        );
    }
    Ok(())
}
