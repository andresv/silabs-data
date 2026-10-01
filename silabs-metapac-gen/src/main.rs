//! Build the `silabs-metapac` crate from per-chip JSON and the curated
//! `data/registers/<kind>_<version>.yaml` IR snapshots.
//!
//! - `gen` renders the crate into `--out-dir`. It reads `data/registers/`
//!   and never writes there.
//! - `seed` is a one-shot bootstrap. It extracts every peripheral from the
//!   SVDs, applies `transforms/<BLOCK>.yaml`, and groups the IRs by the
//!   perimap `(kind, register_version, block)`. It writes one
//!   `data/registers/<kind>_<version>.yaml` per `(kind, version)`.
//!   Instances that only add registers, fields or enum values merge into a
//!   superset. A real conflict (a name that moves or changes width) stops
//!   the command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use silabs_data_gen::chips::ChipFile;
use silabs_data_gen::perimap;
use silabs_metapac_gen::codegen::{self, GenerateInput};
use silabs_metapac_gen::pac::{self, IpKey, module_name};
use silabs_metapac_gen::seed_merge::{combine_blocks, merge_superset};
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

    /// One-shot bootstrap of `data/registers/<kind>_<version>.yaml` from the
    /// SVDs. Stops at the first conflict between two instances of one block,
    /// unless `--candidates-dir` is set.
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
        /// Only seed chips whose name matches this regex (e.g.
        /// `^EFM32GG[0-9]{3}F`). Buckets used only by other chips are not
        /// written, so their curated YAMLs stay untouched.
        #[arg(long)]
        chips: Option<String>,
        /// On `(kind, version)` divergence, write every distinct IR to
        /// `<dir>/<kind>_<version>/<hash>.yaml` plus `index.txt` (which chip
        /// peripherals produced each hash), instead of exiting at the first
        /// conflict. Diverged buckets are not written to the registers dir,
        /// and the command exits non-zero after listing them.
        #[arg(long)]
        candidates_dir: Option<PathBuf>,
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
            chips,
            candidates_dir,
        } => run_seed(
            &pack,
            &data_dir,
            &transforms_dir,
            &registers_yaml_dir,
            chips.as_deref(),
            candidates_dir.as_deref(),
        ),
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
    for chip in &chips {
        crate_layout::validate_trustzone_aliases(chip)
            .with_context(|| format!("validate TrustZone aliases for {}", chip.chip.name))?;
    }

    let extract_dirs = if !packs.is_empty() {
        pack_extract_dirs(packs)?
    } else {
        Vec::new()
    };
    let only_set: BTreeSet<String> = only.iter().map(|s| s.to_ascii_lowercase()).collect();

    // Series 2 marks each banked peripheral with `#define <PERI>_HAS_SET_CLEAR`
    // in its CMSIS device header. Reading the packs instead of a fixed list
    // picks up the banked kinds of new families.
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

    // Collect every (kind, version) referenced by any chip.
    let mut module_users: BTreeMap<IpKey, BTreeSet<String>> = BTreeMap::new();
    for chip in &chips {
        let feat = crate_layout::feature_name(&chip.chip.name);
        let names: BTreeSet<&str> = chip.peripherals.iter().map(|p| p.name.as_str()).collect();
        for p in &chip.peripherals {
            if peripheral::secure_to_nonsecure_name(&p.name).is_some_and(|peer| names.contains(peer.as_str())) {
                continue;
            }
            let key: IpKey = (p.kind.clone(), p.register_version.clone());
            module_users.entry(key).or_default().insert(feat.clone());
        }
    }

    let banked_keys = silabs_metapac_gen::expand_aliases::banked_keys(&chips, &banked_kinds);

    // Load `data/registers/<kind>_<version>.yaml` for each key.
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
        // Add the `_SET`/`_CLR`/`_TGL` alias views at +0x1000/+0x2000/+0x3000.
        // The SVD and the YAML carry only the base layout.
        if banked_keys.contains(key) {
            silabs_metapac_gen::expand_aliases::expand_series2_aliases(&mut ir);
        }
        irs.insert(key.clone(), ir);
    }

    std::fs::create_dir_all(out_dir.join("src/chips"))
        .with_context(|| format!("create out dir {}", out_dir.display()))?;
    pac::write_peripherals_dir(&irs, &out_dir.join("src/peripherals"))?;
    pac::write_common_module(&out_dir.join("src/common.rs"))?;

    silabs_metapac_gen::ir_metadata::write_metadata_module(out_dir)?;
    silabs_metapac_gen::ir_metadata::write_registers_dir(&irs, &out_dir.join("src/registers"))?;

    // Cargo.toml + lib.rs.
    let chip_features: Vec<String> = chips.iter().map(|c| crate_layout::feature_name(&c.chip.name)).collect();
    crate_layout::write_cargo_toml(&chip_features, &out_dir.join("Cargo.toml"))?;
    crate_layout::write_build_rs(&out_dir.join("build.rs"))?;

    crate_layout::write_lib_rs(&out_dir.join("src/lib.rs"))?;
    crate_layout::write_all_tables(&chips, &out_dir.join("src"))?;
    silabs_metapac_gen::cfgs::write_check_cfgs(&chips, &out_dir.join("src/check_cfgs.txt"))?;

    std::fs::write(
        out_dir.join("README.md"),
        "# silabs-metapac\n\n\
         Generated Silicon Labs PAC. Do not edit by hand — regenerate via `silabs-metapac-gen`.\n",
    )?;

    // Per-chip mod.rs + device.x.
    for chip in &chips {
        let feat = crate_layout::feature_name(&chip.chip.name);
        let chip_dir = out_dir.join("src/chips").join(&feat);
        std::fs::create_dir_all(&chip_dir)?;

        let gpio_ports = crate_layout::gpio_port_count(chip, &irs)
            .with_context(|| format!("GPIO port count for {}", chip.chip.name))?;
        let pac_rs = crate_layout::build_chip_pac_rs(chip, gpio_ports);
        std::fs::write(chip_dir.join("pac.rs"), pac_rs)?;

        // HAL build scripts read the chip metadata to generate singletons.
        let names = crate_layout::canonical_peripheral_names(chip);
        let cmu = silabs_metapac_gen::clocks::peripheral_cmu(chip, &names, &irs)
            .with_context(|| format!("CMU data for {}", chip.chip.name))?;
        let metadata_rs = crate_layout::build_chip_metadata_rs(chip, &cmu, gpio_ports)
            .with_context(|| format!("metadata for {}", chip.chip.name))?;
        std::fs::write(chip_dir.join("metadata.rs"), metadata_rs)?;
        std::fs::write(chip_dir.join("memory.x"), crate_layout::build_memory_x(chip)?)?;
        silabs_metapac_gen::cfgs::write_chip_cfgs(chip, &chip_dir.join("cfgs.txt"))?;

        let device_x_path = chip_dir.join("device.x");
        let render_device_x = !extract_dirs.is_empty() && (only_set.is_empty() || only_set.contains(&feat));
        if render_device_x {
            let svd_path = extract_dirs
                .iter()
                .map(|d| d.join(&chip.chip.svd))
                .find(|p| p.is_file())
                .ok_or_else(|| anyhow!("SVD {} missing for {}", chip.chip.svd, chip.chip.name))?;
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

/// Conflict report for `seed`.
#[allow(clippy::too_many_arguments)]
fn bail_divergence(
    key: &IpKey,
    block: &str,
    a_chip: &str,
    a_peripheral: &str,
    a_hash: &str,
    b_chip: &str,
    b_peripheral: &str,
    b_hash: &str,
    why: &str,
) -> ! {
    eprintln!();
    eprintln!(
        "=== seed conflict: (kind={}, version={}, block={block}) ===",
        key.0, key.1
    );
    eprintln!("  {a_chip} :: {a_peripheral}  →  IR hash {a_hash}");
    eprintln!("  {b_chip} :: {b_peripheral}  →  IR hash {b_hash}");
    eprintln!("  {why}");
    eprintln!();
    eprintln!("Both peripherals route to the same (kind, version, block) but their");
    eprintln!("IRs conflict: a name moves or changes width, so neither is a superset");
    eprintln!("of the other. Resolve via one of:");
    eprintln!("  1. write a transforms/<BLOCK>.yaml rule that normalises both");
    eprintln!("     extractions to one canonical shape;");
    eprintln!(
        "  2. hand-curate data/registers/{}.yaml with the canonical shape;",
        module_name(&key.0, &key.1)
    );
    eprintln!("  3. add a perimap entry in silabs-data-gen/src/perimap.rs that routes");
    eprintln!("     one of these peripherals to a distinct version label or block.");
    std::process::exit(1);
}

fn run_seed(
    packs: &[PathBuf],
    data_dir: &Path,
    transforms_dir: &Path,
    registers_yaml_dir: &Path,
    chips_filter: Option<&str>,
    candidates_dir: Option<&Path>,
) -> Result<()> {
    if packs.is_empty() {
        bail!("at least one --pack is required for seed");
    }
    let extract_dirs = pack_extract_dirs(packs)?;
    let chips = load_chips(&data_dir.join("chips"))?;
    let chips_re = chips_filter
        .map(regex::Regex::new)
        .transpose()
        .context("--chips regex")?;
    let chips: Vec<ChipFile> = chips
        .into_iter()
        .filter(|c| chips_re.as_ref().is_none_or(|re| re.is_match(&c.chip.name)))
        .collect();
    if chips.is_empty() {
        bail!("--chips {chips_filter:?} matched no chip JSON");
    }
    eprintln!("Seeding {} chips across {} packs", chips.len(), packs.len());

    let cfg = svd_parser::Config::default()
        .expand_properties(true)
        .validate_level(ValidateLevel::Disabled);

    // (kind, version, block) → (superset ir, hash of the first ir, first-claiming chip / peripheral).
    struct Bucket {
        ir: chiptool::ir::IR,
        hash: String,
        chip: String,
        peripheral: String,
    }
    type BlockKey = (String, String, String);
    let mut buckets: BTreeMap<BlockKey, Bucket> = BTreeMap::new();
    // Every distinct IR per bucket, with the chip peripherals that produced
    // it. Only filled when `--candidates-dir` is set.
    let mut variants: BTreeMap<BlockKey, BTreeMap<String, (chiptool::ir::IR, Vec<String>)>> = BTreeMap::new();
    // Buckets whose IRs conflict, and superset merges to report.
    let mut conflicts: BTreeSet<BlockKey> = BTreeSet::new();
    let mut merges: BTreeMap<BlockKey, Vec<String>> = BTreeMap::new();

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
            let key: BlockKey = (inst.kind.clone(), inst.register_version.clone(), inst.block.clone());
            let ir = extract::extract_ip(periph, &inst.block, &inst.register_version, transforms_dir).with_context(
                || {
                    format!(
                        "extract {pname} (kind={}, version={}) from {}",
                        inst.kind, inst.register_version, chip.chip.name
                    )
                },
            )?;
            let hash = ir_hash(&ir);
            if candidates_dir.is_some() {
                variants
                    .entry(key.clone())
                    .or_default()
                    .entry(hash.clone())
                    .or_insert_with(|| (ir.clone(), Vec::new()))
                    .1
                    .push(format!("{} :: {pname}", chip.chip.name));
            }

            match buckets.get_mut(&key) {
                Some(existing) if existing.hash != hash => match merge_superset(&mut existing.ir, &ir) {
                    Ok(0) => {}
                    Ok(n) => merges
                        .entry(key.clone())
                        .or_default()
                        .push(format!("{} :: {pname} adds {n}", chip.chip.name)),
                    Err(why) if candidates_dir.is_none() => bail_divergence(
                        &(key.0.clone(), key.1.clone()),
                        &key.2,
                        &existing.chip,
                        &existing.peripheral,
                        &existing.hash,
                        &chip.chip.name,
                        pname,
                        &hash,
                        &why,
                    ),
                    Err(_) => {
                        conflicts.insert(key.clone());
                    }
                },
                Some(_) => {}
                None => {
                    buckets.insert(
                        key,
                        Bucket {
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

    let diverged: BTreeSet<IpKey> = conflicts.iter().map(|k| (k.0.clone(), k.1.clone())).collect();
    if let Some(dir) = candidates_dir {
        for (key, by_hash) in &variants {
            if !conflicts.contains(key) {
                continue;
            }
            let kdir = dir.join(format!("{}_{}", module_name(&key.0, &key.1), key.2));
            std::fs::create_dir_all(&kdir).with_context(|| format!("create {}", kdir.display()))?;
            let mut index = String::new();
            for (hash, (ir, users)) in by_hash {
                std::fs::write(kdir.join(format!("{hash}.yaml")), serde_yaml::to_string(ir)?)?;
                index.push_str(&format!("{hash}.yaml\n"));
                for u in users {
                    index.push_str(&format!("  {u}\n"));
                }
            }
            std::fs::write(kdir.join("index.txt"), index)?;
        }
    }

    for (key, notes) in &merges {
        eprintln!(
            "  merged into superset: {} block {}",
            module_name(&key.0, &key.1),
            key.2
        );
        for n in notes {
            eprintln!("    {n}");
        }
    }

    // One YAML per (kind, version) with every block of that version in it.
    let mut by_version: BTreeMap<IpKey, Vec<(String, chiptool::ir::IR)>> = BTreeMap::new();
    for (key, bucket) in &buckets {
        let top = bucket
            .ir
            .blocks
            .keys()
            .find(|b| {
                !bucket.ir.blocks.values().any(|o| {
                    o.items
                        .iter()
                        .any(|i| matches!(&i.inner, chiptool::ir::BlockItemInner::Block(x) if &x.block == *b))
                })
            })
            .with_context(|| format!("no top-level block in {}_{} {}", key.0, key.1, key.2))?
            .clone();
        by_version
            .entry((key.0.clone(), key.1.clone()))
            .or_default()
            .push((top, bucket.ir.clone()));
    }

    std::fs::create_dir_all(registers_yaml_dir).with_context(|| format!("create {}", registers_yaml_dir.display()))?;
    for (key, parts) in by_version.iter().filter(|(k, _)| !diverged.contains(*k)) {
        let ir = combine_blocks(parts.clone())
            .with_context(|| format!("combine blocks of {}", module_name(&key.0, &key.1)))?;
        let fname = format!("{}.yaml", module_name(&key.0, &key.1));
        let path = registers_yaml_dir.join(&fname);
        let mut f = std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        serde_yaml::to_writer(&mut f, &ir).with_context(|| format!("serialise IR to {}", path.display()))?;
    }
    let _ = perimap::compile()?;
    eprintln!(
        "Wrote {} register YAMLs to {}",
        by_version.len() - diverged.len(),
        registers_yaml_dir.display()
    );
    // Show the chip that each YAML was first extracted from, for hand curation.
    for (key, bucket) in &buckets {
        if diverged.contains(&(key.0.clone(), key.1.clone())) {
            continue;
        }
        eprintln!(
            "  {}_{}.yaml  block={}  seeded from {} :: {}",
            key.0, key.1, key.2, bucket.chip, bucket.peripheral
        );
    }
    if !diverged.is_empty() {
        for key in &diverged {
            eprintln!("  diverged: {}", module_name(&key.0, &key.1));
        }
        bail!(
            "{} bucket(s) diverged; candidates written to {}",
            diverged.len(),
            candidates_dir.expect("diverged implies candidates_dir").display()
        );
    }
    Ok(())
}
