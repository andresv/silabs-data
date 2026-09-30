//! Assemble a `silabs-metapac` Cargo crate from per-chip JSON files.
//!
//! Layout produced:
//!
//! ```text
//! silabs-metapac/
//! ├── Cargo.toml
//! ├── README.md
//! ├── build.rs
//! └── src/
//!     ├── lib.rs
//!     ├── common.rs
//!     ├── metadata.rs, all_chips.rs, all_peripheral_versions.rs, check_cfgs.txt
//!     ├── peripherals/<kind>_<version>.rs   # chiptool register API
//!     ├── registers/<kind>_<version>.rs     # IR statics for `metadata`
//!     └── chips/
//!         └── <chip>/
//!             ├── pac.rs        # peripheral instances + interrupts + memory map
//!             ├── metadata.rs   # `METADATA` static
//!             ├── device.x      # cortex-m-rt linker fragment (or stub)
//!             └── cfgs.txt
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use convert_case::{Boundary, Case, Casing};
use silabs_data_gen::chips::{ChipFile, Interrupt, PeripheralInstance};
use silabs_data_gen::pdsc::MemoryRegion;

use crate::pac::{IpKey, module_name};
use crate::peripheral::{nonsecure_to_secure_name, secure_to_nonsecure_name};

/// Convert a perimap-routed block name (e.g. `GPIO`, `EUSART`, `I2C`) into the
/// PascalCase identifier `chiptool::transform::sanitize::Sanitize::default()`
/// produces (e.g. `Gpio`, `Eusart`, `I2c`).
///
/// Like chiptool's `sanitize_with_case`, it removes digit boundaries first.
/// Without that step `I2C` stays `I2C` and misses the struct `I2c`.
fn block_struct_ident(block: &str) -> String {
    block.remove_boundaries(&Boundary::digits()).to_case(Case::Pascal)
}

/// Secure aliases whose non-secure peer is present in the same chip. Only
/// these confirmed pairs may share the non-secure peer's register definition.
fn paired_secure_alias_names(peripherals: &[PeripheralInstance]) -> BTreeSet<&str> {
    let names: BTreeSet<&str> = peripherals.iter().map(|p| p.name.as_str()).collect();
    peripherals
        .iter()
        .filter_map(|p| {
            secure_to_nonsecure_name(&p.name)
                .filter(|peer| names.contains(peer.as_str()))
                .map(|_| p.name.as_str())
        })
        .collect()
}

struct PeripheralGroup<'a> {
    canonical_name: String,
    nonsecure: &'a PeripheralInstance,
    secure: Option<&'a PeripheralInstance>,
}

/// Collapse each confirmed NS/S pair to one register-layout owner while
/// retaining the original secure instance (and address) alongside it.
fn peripheral_groups(peripherals: &[PeripheralInstance]) -> BTreeMap<String, PeripheralGroup<'_>> {
    let by_name: BTreeMap<&str, &PeripheralInstance> = peripherals.iter().map(|p| (p.name.as_str(), p)).collect();
    let paired_secure = paired_secure_alias_names(peripherals);
    let mut groups = BTreeMap::new();

    for p in peripherals {
        if paired_secure.contains(p.name.as_str()) {
            continue;
        }

        let secure = nonsecure_to_secure_name(&p.name)
            .and_then(|name| by_name.get(name.as_str()).copied())
            .filter(|candidate| paired_secure.contains(candidate.name.as_str()));
        // Preserve the existing public name for suffix aliases (`GPIO_NS` →
        // `GPIO`). Infix aliases were already public as `SEMAILBOX_NS_HOST`.
        let canonical_name = p
            .name
            .strip_suffix("_NS")
            .map(str::to_owned)
            .unwrap_or_else(|| p.name.clone());
        let old = groups.insert(
            canonical_name.clone(),
            PeripheralGroup {
                canonical_name,
                nonsecure: p,
                secure,
            },
        );
        assert!(old.is_none(), "duplicate canonical peripheral name in chip data");
    }
    groups
}

/// Check that the two addresses of each Series 2 NS/S pair differ by
/// `0x1000_0000`.
///
/// The addresses come from the SVD. The check makes a changed vendor
/// map fail generation instead of producing a misleading API.
pub fn validate_trustzone_aliases(chip: &ChipFile) -> Result<()> {
    let groups = peripheral_groups(&chip.peripherals);
    for group in groups.values() {
        let Some(secure) = group.secure else {
            continue;
        };
        let nonsecure = group.nonsecure;

        if chip.chip.series.as_ref().is_some_and(|s| s.series == 2)
            && nonsecure.base_address ^ secure.base_address != 0x1000_0000
        {
            bail!(
                "{}: TrustZone aliases {}=0x{:08X} and {}=0x{:08X} do not differ by 0x1000_0000",
                chip.chip.name,
                nonsecure.name,
                nonsecure.base_address,
                secure.name,
                secure.base_address,
            );
        }
    }
    Ok(())
}

/// Lower-cased Cargo feature name for a given chip name (`EFR32MG26B211F2048IM68`).
pub fn feature_name(chip: &str) -> String {
    chip.to_ascii_lowercase()
}

/// Write `build.rs` that adds the active chip's source directory to the
/// linker search path under the `rt` feature, and passes the chip's cfgs
/// to direct dependents through `links` metadata (see [`crate::cfgs`]).
///
/// `cortex-m-rt`'s `link.x` does `INCLUDE device.x`, and `silabs-metapac`
/// emits a per-chip `device.x` into `src/chips/<chip>/`. Without this
/// helper the linker can't find it. Mirrors the analogous build script
/// in `stm32-metapac`.
pub fn write_build_rs(out: &Path) -> Result<()> {
    let s = r##"use std::env;
use std::path::{Path, PathBuf};

enum GetOneError {
    None,
    Multiple,
}

trait IteratorExt: Iterator {
    fn get_one(self) -> Result<Self::Item, GetOneError>;
}

impl<T: Iterator> IteratorExt for T {
    fn get_one(mut self) -> Result<Self::Item, GetOneError> {
        match self.next() {
            None => Err(GetOneError::None),
            Some(res) => match self.next() {
                Some(_) => Err(GetOneError::Multiple),
                None => Ok(res),
            },
        }
    }
}

/// Read a generated one-item-per-line list and join it with spaces.
fn read_list(path: &Path) -> String {
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

fn main() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let chip_name = match env::vars()
        .map(|(a, _)| a)
        .filter(|x| x.starts_with("CARGO_FEATURE_EFR32") || x.starts_with("CARGO_FEATURE_EFM32"))
        .get_one()
    {
        Ok(x) => x,
        Err(GetOneError::None) => panic!("No silabs-metapac chip feature enabled (e.g. --features efr32mg26b211f2048im68)"),
        Err(GetOneError::Multiple) => panic!("Multiple silabs-metapac chip features enabled — pick one"),
    }
    .strip_prefix("CARGO_FEATURE_")
    .unwrap()
    .to_ascii_lowercase();

    #[cfg(feature = "rt")]
    println!(
        "cargo:rustc-link-search={}/src/chips/{}",
        crate_dir.display(),
        chip_name,
    );

    // Give `lib.rs` the chip's pac.rs and metadata.rs paths, so it needs one
    // `include!(env!(...))` and not one cfg-gated `include!` per chip.
    println!("cargo:rustc-env=SILABS_METAPAC_PAC_PATH=chips/{}/pac.rs", chip_name);
    println!(
        "cargo:rustc-env=SILABS_METAPAC_METADATA_PATH=chips/{}/metadata.rs",
        chip_name
    );

    // `links = "silabs-metapac"` metadata. Cargo passes each key to the
    // build script of every direct dependent as `DEP_SILABS_METAPAC_<KEY>`.
    // A dependent re-emits `cfgs` as `cargo:rustc-cfg` and `check_cfgs` as
    // `cargo:rustc-check-cfg`. Then it can use `#[cfg(letimer_s0v1)]` without
    // its own chip features. Both lists are space-separated.
    println!("cargo::metadata=chip={chip_name}");
    println!(
        "cargo::metadata=cfgs={}",
        read_list(&crate_dir.join("src/chips").join(&chip_name).join("cfgs.txt"))
    );
    println!("cargo::metadata=check_cfgs={}", read_list(&crate_dir.join("src/check_cfgs.txt")));

    println!("cargo:rerun-if-changed=build.rs");
}
"##;
    std::fs::write(out, s).with_context(|| format!("write build.rs at {}", out.display()))?;
    Ok(())
}

fn render_cargo_toml(chip_features: &[String]) -> String {
    let mut s = include_str!("../res/Cargo.toml").to_owned();
    for feature in chip_features {
        writeln!(&mut s, "{feature} = []").expect("writing to a String cannot fail");
    }
    s
}

/// Write the publish-ready Cargo.toml with one boolean feature per chip OPN.
pub fn write_cargo_toml(chip_features: &[String], out: &Path) -> Result<()> {
    let s = render_cargo_toml(chip_features);
    std::fs::write(out, s).with_context(|| format!("write Cargo.toml at {}", out.display()))?;
    Ok(())
}

/// Write src/lib.rs.
pub fn write_lib_rs(out: &Path) -> Result<()> {
    let mut s = String::new();
    s.push_str(
        r#"#![no_std]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(clippy::all)]
#![allow(unused)]

"#,
    );

    // The module declarations and typed peripheral consts are in
    // `chips/<chip>/pac.rs` and `chips/<chip>/metadata.rs`, which the
    // `build.rs` env vars select. Included tokens keep their `Span`, so
    // `#[path]` in those files resolves relative to the chip directory.
    //
    // `build.rs` panics on zero or several chip features, so lib.rs needs no
    // `compile_error!`.
    s.push_str("pub mod common;\n\n");

    s.push_str("#[cfg(feature = \"pac\")]\n");
    s.push_str("include!(env!(\"SILABS_METAPAC_PAC_PATH\"));\n\n");

    s.push_str("#[cfg(feature = \"metadata\")]\n");
    s.push_str("pub mod metadata {\n");
    s.push_str("    include!(\"metadata.rs\");\n");
    s.push_str("    include!(env!(\"SILABS_METAPAC_METADATA_PATH\"));\n");
    s.push_str("    include!(\"all_chips.rs\");\n");
    s.push_str("    include!(\"all_peripheral_versions.rs\");\n");
    s.push_str("}\n");

    std::fs::write(out, s).with_context(|| format!("write lib.rs at {}", out.display()))?;
    Ok(())
}

/// Write `src/all_chips.rs` and `src/all_peripheral_versions.rs`, included
/// into `pub mod metadata`. A HAL build script uses them to declare every
/// chip and `<kind>_<version>` cfg for `rustc-check-cfg`, not only the active
/// chip's.
pub fn write_all_tables(chips: &[ChipFile], src_dir: &Path) -> Result<()> {
    let mut s = String::from("pub static ALL_CHIPS: &[&str] = &[\n");
    for chip in chips {
        writeln!(&mut s, "    {:?},", chip.chip.name).expect("writing to a String cannot fail");
    }
    s.push_str("];\n");
    let out = src_dir.join("all_chips.rs");
    std::fs::write(&out, s).with_context(|| format!("write {}", out.display()))?;

    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for chip in chips {
        for p in &chip.peripherals {
            versions.entry(&p.kind).or_default().insert(&p.register_version);
        }
    }
    let mut s = String::from("pub static ALL_PERIPHERAL_VERSIONS: &[(&str, &[&str])] = &[\n");
    for (kind, vs) in &versions {
        let vs: Vec<String> = vs.iter().map(|v| format!("{v:?}")).collect();
        writeln!(&mut s, "    ({kind:?}, &[{}]),", vs.join(", ")).expect("writing to a String cannot fail");
    }
    s.push_str("];\n");
    let out = src_dir.join("all_peripheral_versions.rs");
    std::fs::write(&out, s).with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

/// Build the `chips/<chip>/pac.rs` content from a parsed ChipFile.
///
/// Each peripheral instance carries the `(kind, register_version, block)`
/// that perimap gave it in `silabs-data-gen gen`.
pub fn build_chip_pac_rs(chip: &ChipFile, gpio_ports: Option<usize>) -> String {
    let mut s = String::new();
    s.push_str("// Per-chip PAC content: peripheral module decls, typed peripheral\n");
    s.push_str("// consts, interrupt enum + cortex-m-rt glue, memory map.\n");
    s.push_str(&format!("// Generated for {}.\n//\n", chip.chip.name));
    s.push_str("// `lib.rs` includes this file at the crate root, through the\n");
    s.push_str("// `SILABS_METAPAC_PAC_PATH` env var from `build.rs`. `#[path]`\n");
    s.push_str("// resolves relative to this file, so `../../peripherals/...`\n");
    s.push_str("// reaches the shared chiptool modules in `src/peripherals/`.\n\n");

    // Declare only the (kind, version) pairs this chip uses. Module names keep
    // the version because one die can have two versions of a kind (EFR32MG26:
    // `eusart_s2v2` and `eusart_s2v2_lf`).
    let paired_secure = paired_secure_alias_names(&chip.peripherals);
    let mut kinds: BTreeSet<(String, String)> = BTreeSet::new();
    for p in &chip.peripherals {
        if paired_secure.contains(p.name.as_str()) {
            continue;
        }
        kinds.insert((p.kind.clone(), p.register_version.clone()));
    }
    if !kinds.is_empty() {
        s.push_str("// Chiptool peripheral modules (shared register/field types).\n");
        for (kind, version) in &kinds {
            let mod_name = format!("{kind}_{version}");
            s.push_str(&format!(
                "#[path = \"../../peripherals/{mod_name}.rs\"]\npub mod {mod_name};\n"
            ));
        }
        s.push_str("\n");

        // Version-neutral aliases (`pub use cmu_s2v3 as cmu;`), emitted when the
        // chip's non-secure peripherals agree on one version of a kind.
        let mut ns_versions: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
        for p in &chip.peripherals {
            if paired_secure.contains(p.name.as_str()) {
                continue;
            }
            ns_versions.entry(&p.kind).or_default().insert(&p.register_version);
        }
        let aliased: Vec<_> = ns_versions
            .iter()
            .filter_map(|(kind, versions)| match versions.first() {
                Some(version) if versions.len() == 1 => Some((kind, version)),
                _ => None,
            })
            .collect();
        if !aliased.is_empty() {
            s.push_str("// Version-neutral aliases for single-version kinds.\n");
            for (kind, version) in aliased {
                let mod_name = module_name(kind, version);
                s.push_str(&format!("pub use {mod_name} as {kind};\n"));
            }
            s.push('\n');
        }
    }

    s.push_str("/// Memory map (flash/RAM regions, from the CMSIS pdsc).\n");
    s.push_str("pub mod memory {\n");
    for m in &chip.chip.memory {
        emit_memory_consts(&mut s, m);
    }
    s.push_str("}\n\n");

    s.push_str("/// Typed peripheral instance constants.\n");
    s.push_str("///\n");
    s.push_str("/// The canonical unsuffixed name uses the non-secure address for a paired\n");
    s.push_str("/// TrustZone peripheral, and an explicit `_S` constant uses the secure SVD\n");
    s.push_str("/// address. Infix vendor names such as `_NS_HOST` remain unchanged.\n");
    emit_typed_peripheral_consts(&mut s, &chip.peripherals);

    emit_gpio_port_constants(&mut s, gpio_ports);

    // Interrupts exist only as `pub enum Interrupt`. `Interrupt::FOO as u16`
    // gives the number.
    let nvic_prio_bits = chip
        .chip
        .nvic_prio_bits
        .expect("chip.nvic_prio_bits missing — re-run silabs-data-gen to populate it");
    emit_cortex_m_rt_glue(&mut s, &chip.interrupts, nvic_prio_bits);

    s
}

fn emit_cortex_m_rt_glue(s: &mut String, interrupts: &[Interrupt], nvic_prio_bits: u8) {
    let mut by_value: std::collections::BTreeMap<u32, &Interrupt> = std::collections::BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for i in interrupts {
        if !seen.insert(i.name.as_str()) {
            continue;
        }
        by_value.insert(i.value, i);
    }

    let max_value = by_value.keys().copied().max().unwrap_or(0);
    let len = (max_value as usize) + 1;

    s.push_str("#[derive(Copy, Clone, Debug, PartialEq, Eq)]\n");
    s.push_str("#[cfg_attr(feature = \"defmt\", derive(defmt::Format))]\n");
    s.push_str("#[repr(u16)]\n");
    s.push_str("pub enum Interrupt {\n");
    for (v, i) in &by_value {
        if let Some(d) = &i.description {
            let d = d.replace('\n', " ").replace('\r', "");
            s.push_str(&format!("    /// {v} - {d}\n"));
        }
        s.push_str(&format!("    {} = {v},\n", i.name));
    }
    s.push_str("}\n\n");

    s.push_str("unsafe impl cortex_m::interrupt::InterruptNumber for Interrupt {\n");
    s.push_str("    #[inline(always)]\n");
    s.push_str("    fn number(self) -> u16 { self as u16 }\n");
    s.push_str("}\n\n");

    s.push_str("#[cfg(feature = \"rt\")]\n");
    s.push_str("mod _vectors {\n");
    s.push_str("    unsafe extern \"C\" {\n");
    for i in by_value.values() {
        s.push_str(&format!("        fn {}();\n", i.name));
    }
    s.push_str("    }\n\n");
    s.push_str("    pub union Vector {\n");
    s.push_str("        _handler: unsafe extern \"C\" fn(),\n");
    s.push_str("        _reserved: u32,\n");
    s.push_str("    }\n\n");
    s.push_str("    #[unsafe(link_section = \".vector_table.interrupts\")]\n");
    s.push_str("    #[unsafe(no_mangle)]\n");
    s.push_str(&format!("    pub static __INTERRUPTS: [Vector; {len}] = [\n"));
    for v in 0..len as u32 {
        match by_value.get(&v) {
            Some(i) => s.push_str(&format!("        Vector {{ _handler: {} }},\n", i.name)),
            None => s.push_str("        Vector { _reserved: 0 },\n"),
        }
    }
    s.push_str("    ];\n");
    s.push_str("}\n\n");

    // Not gated on `rt`: crates that set NVIC priorities (rtic-monotonics)
    // need it without owning the vector table.
    s.push_str("/// Number available in the NVIC for configuring priority.\n");
    s.push_str(&format!("pub const NVIC_PRIO_BITS: u8 = {nvic_prio_bits};\n\n"));

    s.push_str("#[cfg(feature = \"rt\")]\n");
    s.push_str("pub use cortex_m_rt::interrupt;\n");
    s.push_str("#[cfg(feature = \"rt\")]\n");
    s.push_str("pub use Interrupt as interrupt;\n");
}

fn emit_memory_consts(s: &mut String, m: &MemoryRegion) {
    let id = m.id.to_ascii_uppercase();
    s.push_str(&format!("    pub const {id}_BASE: usize = 0x{:08X};\n", m.start));
    s.push_str(&format!("    pub const {id}_SIZE: usize = 0x{:08X};\n", m.size));
}

/// Emit typed peripheral instance consts. A confirmed NS/S pair shares the
/// non-secure peer's register-block type but keeps both exact SVD addresses.
/// A suffix pair gets the unsuffixed non-secure const and an `_S` const, but
/// no `_NS` const.
fn emit_typed_peripheral_consts(s: &mut String, peripherals: &[PeripheralInstance]) {
    fn emit_const(s: &mut String, name: &str, type_owner: &PeripheralInstance, address: u64) {
        let mod_name = module_name(&type_owner.kind, &type_owner.register_version);
        let struct_name = block_struct_ident(&type_owner.block);
        s.push_str(&format!(
            "pub const {name}: crate::{mod_name}::{struct_name} = unsafe {{ \
             crate::{mod_name}::{struct_name}::from_ptr(0x{address:08X} as *mut ()) }};\n",
        ));
    }

    for group in peripheral_groups(peripherals).into_values() {
        let p = group.nonsecure;
        emit_const(s, &group.canonical_name, p, p.base_address);
        if let Some(secure) = group.secure {
            emit_const(s, &secure.name, p, secure.base_address);
        }
    }
    s.push('\n');
}

/// Number of GPIO ports on a chip: the `p_ctrl` register-array length in
/// its curated GPIO block. `None` when the chip has no GPIO peripheral.
pub fn gpio_port_count(chip: &ChipFile, irs: &BTreeMap<IpKey, chiptool::ir::IR>) -> Result<Option<usize>> {
    let Some(gpio) = chip.peripherals.iter().find(|p| p.kind == "gpio") else {
        return Ok(None);
    };
    let mod_name = module_name(&gpio.kind, &gpio.register_version);
    let ir = irs
        .get(&(gpio.kind.clone(), gpio.register_version.clone()))
        .ok_or_else(|| anyhow!("no IR loaded for {mod_name}"))?;
    for block in ir.blocks.values() {
        if let Some(item) = block.items.iter().find(|i| i.name == "p_ctrl") {
            let Some(array) = &item.array else {
                bail!("{mod_name}: p_ctrl is not a register array");
            };
            return Ok(Some(array.len()));
        }
    }
    bail!("{mod_name}: GPIO block has no p_ctrl register array")
}

fn emit_gpio_port_constants(s: &mut String, gpio_ports: Option<usize>) {
    let Some(n) = gpio_ports else {
        return;
    };
    s.push_str("/// GPIO port indices, as in the CMSIS `<family>_gpio.h`\n");
    s.push_str("/// `#define GPIO_PORTA 0`. Use as `GPIO.p_ctrl(gpio_port::PORTC)`.\n");
    s.push_str("pub mod gpio_port {\n");
    for i in 0..n {
        let ch = (b'A' + i as u8) as char;
        s.push_str(&format!("    pub const PORT{ch}: usize = {i};\n"));
    }
    s.push_str("}\n\n");
}

/// Stub `device.x` placeholder.
pub fn stub_device_x(chip_name: &str) -> String {
    format!("/* device.x for {chip_name} not yet generated */\n")
}

/// The `Series::Series<N>(<M>)` literal for a chip, from the
/// `_SILICON_LABS_32B_SERIES_<N>_CONFIG_<M>` macros in its CMSIS device
/// header (see [`silabs_data_gen::header::extract_series`]).
///
/// Panics when the chip JSON has no `Chip.series` (run `./d gen-all`), or
/// when the series is not 0 to 3, the range of the metapac `Series` enum.
fn series_literal_for_chip(chip: &ChipFile) -> String {
    let s = chip
        .chip
        .series
        .expect("chip.series missing — re-run silabs-data-gen to populate it");
    match s.series {
        0 => "Series::Series0".to_string(),
        1 => {
            let cfg: u8 = s.config.try_into().expect("Series 1 config fits in u8 (1..=4)");
            format!("Series::Series1({cfg})")
        }
        2 => {
            let cfg: u8 = s.config.try_into().expect("Series 2 config fits in u8 (1..=9)");
            format!("Series::Series2({cfg})")
        }
        3 => format!("Series::Series3({})", s.config),
        n => panic!("unsupported chip series {n} — extend `Series` enum in silabs-metapac-gen/res/metadata.rs"),
    }
}

/// Build the `chips/<chip>/metadata.rs` content from a parsed `ChipFile`.
///
/// The file holds `pub static METADATA: Metadata`. It is included into the
/// crate's `pub mod metadata` (see [`write_lib_rs`]), so `Metadata`,
/// `Peripheral` and the other type names resolve without a `use`.
///
/// One metadata row owns each register layout. For a paired TrustZone
/// peripheral it carries both the non-secure and secure SVD base addresses.
pub fn build_chip_metadata_rs(chip: &ChipFile) -> String {
    let mut s = String::new();
    s.push_str("// Per-chip metadata. Generated for ");
    s.push_str(&chip.chip.name);
    s.push_str(".\n//\n");
    s.push_str("// Included from `pub mod metadata` in the metapac crate root;\n");
    s.push_str("// type names resolve to the surrounding module — see\n");
    s.push_str("// silabs-metapac-gen/res/metadata.rs.\n\n");

    let peripheral_groups = peripheral_groups(&chip.peripherals);

    // Interrupts: dedup by name, preserve value ordering.
    let mut seen_irq: BTreeSet<String> = BTreeSet::new();
    let mut unique_irqs: Vec<&Interrupt> = Vec::new();
    for i in &chip.interrupts {
        if seen_irq.insert(i.name.clone()) {
            unique_irqs.push(i);
        }
    }

    s.push_str("pub static METADATA: Metadata = Metadata {\n");
    s.push_str(&format!("    name: {:?},\n", chip.chip.name));
    s.push_str(&format!("    core: {:?},\n", chip.chip.core));
    s.push_str(&format!("    fpu: {},\n", chip.chip.fpu));
    s.push_str(&format!("    mpu: {},\n", chip.chip.mpu));
    s.push_str(&format!("    trustzone: {},\n", chip.chip.trustzone));
    s.push_str(&format!("    series: {},\n", series_literal_for_chip(chip)));
    let nvic_prio_bits = chip
        .chip
        .nvic_prio_bits
        .expect("chip.nvic_prio_bits missing — re-run silabs-data-gen to populate it");
    s.push_str(&format!("    nvic_priority_bits: {nvic_prio_bits},\n"));

    s.push_str("    memory: &[\n");
    for m in &chip.chip.memory {
        s.push_str(&format!(
            "        MemoryRegion {{ name: {:?}, address: 0x{:08X}, size: 0x{:08X}, access: {:?} }},\n",
            m.id, m.start, m.size, m.access,
        ));
    }
    s.push_str("    ],\n");

    s.push_str("    peripherals: &[\n");
    for group in peripheral_groups.values() {
        let p = group.nonsecure;
        let secure_address = group
            .secure
            .map(|secure| format!("Some(0x{:08X})", secure.base_address))
            .unwrap_or_else(|| "None".to_owned());
        s.push_str(&format!(
            "        Peripheral {{ name: {:?}, address: 0x{:08X}, secure_address: {}, kind: {:?}, version: {:?}, block: {:?} }},\n",
            group.canonical_name, p.base_address, secure_address, p.kind, p.register_version, p.block,
        ));
    }
    s.push_str("    ],\n");

    s.push_str("    interrupts: &[\n");
    for i in &unique_irqs {
        s.push_str(&format!(
            "        Interrupt {{ name: {:?}, number: {} }},\n",
            i.name, i.value,
        ));
    }
    s.push_str("    ],\n");
    s.push_str("};\n\n");

    // Each `<kind>_<version>.rs` holds `pub static REGISTERS: IR`. The chip
    // declares only the kinds it uses. `#[path]` is relative to this file, so
    // `../../registers/...` reaches `src/registers/`.
    let paired_secure = paired_secure_alias_names(&chip.peripherals);
    let mut kinds: BTreeSet<(String, String)> = BTreeSet::new();
    for p in &chip.peripherals {
        if paired_secure.contains(p.name.as_str()) {
            continue;
        }
        kinds.insert((p.kind.clone(), p.register_version.clone()));
    }
    if !kinds.is_empty() {
        s.push_str("// Per-kind IR statics (chiptool IR snapshots).\n");
        for (kind, version) in &kinds {
            let mod_name = format!("{kind}_{version}");
            s.push_str(&format!(
                "#[path = \"../../registers/{mod_name}.rs\"]\npub mod {mod_name};\n"
            ));
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use silabs_data_gen::chips::{Interrupt, PeripheralInstance};
    use silabs_data_gen::pdsc::{Chip, MemoryRegion};

    use super::*;

    fn fake_chip() -> ChipFile {
        ChipFile {
            chip: Chip {
                name: "EFR32MG26B211F2048IM68".into(),
                core: "Cortex-M33".into(),
                fpu: false,
                mpu: false,
                trustzone: false,
                series: Some(silabs_data_gen::header::Series { series: 2, config: 6 }),
                nvic_prio_bits: Some(4),
                memory: vec![
                    MemoryRegion {
                        id: "IROM1".into(),
                        start: 0x0800_0000,
                        size: 0x0020_0000,
                        access: "rx".into(),
                    },
                    MemoryRegion {
                        id: "IRAM1".into(),
                        start: 0x2000_0000,
                        size: 0x0004_0000,
                        access: "rwx".into(),
                    },
                ],
                flash_algo: None,
                svd: "x.svd".into(),
                package: None,
            },
            peripherals: vec![
                PeripheralInstance {
                    name: "ACMP0_NS".into(),
                    base_address: 0x5000_E000,
                    version: Some("2".into()),
                    kind: "acmp".into(),
                    register_version: "s2v2".into(),
                    block: "ACMP".into(),
                },
                PeripheralInstance {
                    name: "ACMP0_S".into(),
                    base_address: 0x4000_E000,
                    version: Some("2".into()),
                    kind: "acmp".into(),
                    register_version: "s2v2".into(),
                    block: "ACMP".into(),
                },
                PeripheralInstance {
                    name: "DCDC".into(),
                    base_address: 0x4000_4000,
                    version: Some("1".into()),
                    kind: "dcdc".into(),
                    register_version: "s2v1".into(),
                    block: "DCDC".into(),
                },
            ],
            interrupts: vec![
                Interrupt {
                    name: "ACMP0".into(),
                    value: 41,
                    description: Some("Analog comparator 0".into()),
                },
                Interrupt {
                    name: "ACMP0".into(),
                    value: 41,
                    description: None,
                },
                Interrupt {
                    name: "TIMER0".into(),
                    value: 25,
                    description: None,
                },
            ],
        }
    }

    #[test]
    fn pac_rs_emits_typed_consts_and_dedupes_interrupts() {
        let s = build_chip_pac_rs(&fake_chip(), None);
        // The chip JSON's `block` field holds the perimap-routed name in raw
        // form (e.g. "ACMP"); `block_struct_ident` Pascal-cases it to match
        // `Sanitize::default()`'s output in the rendered register YAML.
        assert!(
            s.contains(
                "pub const ACMP0: crate::acmp_s2v2::Acmp = unsafe { crate::acmp_s2v2::Acmp::from_ptr(0x5000E000 as *mut ()) };"
            ),
            "missing typed ACMP0 const:\n{s}"
        );
        assert!(!s.contains("pub const ACMP0_NS:"), "redundant ACMP0_NS const:\n{s}");
        assert!(
            s.contains(
                "pub const ACMP0_S: crate::acmp_s2v2::Acmp = unsafe { crate::acmp_s2v2::Acmp::from_ptr(0x4000E000 as *mut ()) };"
            ),
            "missing secure ACMP0_S const:\n{s}"
        );
        assert!(
            s.contains("pub const DCDC: crate::dcdc_s2v1::Dcdc"),
            "missing typed DCDC const:\n{s}"
        );
        // Interrupts exist only as `pub enum Interrupt` variants, with no
        // const module.
        assert!(s.contains("ACMP0 = 41,"), "missing ACMP0 enum variant:\n{s}");
        assert!(s.contains("TIMER0 = 25,"));
        assert!(!s.contains("pub const ACMP0: u8"));
        assert!(!s.contains("pub mod interrupts"));
        assert!(s.contains("IROM1_BASE: usize = 0x08000000"));
        assert!(s.contains("IROM1_SIZE: usize = 0x00200000"));

        // One `#[path]` mod decl for each (kind, version) the chip uses.
        assert!(
            s.contains("#[path = \"../../peripherals/acmp_s2v2.rs\"]\npub mod acmp_s2v2;"),
            "missing acmp_s2v2 #[path] mod decl:\n{s}"
        );
        assert!(
            s.contains("#[path = \"../../peripherals/dcdc_s2v1.rs\"]\npub mod dcdc_s2v1;"),
            "missing dcdc_s2v1 #[path] mod decl:\n{s}"
        );
        assert_eq!(s.matches("pub mod acmp_s2v2;").count(), 1);
    }

    #[test]
    fn pac_rs_emits_version_neutral_kind_aliases() {
        let mut chip = fake_chip();
        // Two versions of the same kind on one die - no alias must be emitted.
        chip.peripherals.push(PeripheralInstance {
            name: "EUSART0_NS".into(),
            base_address: 0x5000_0000,
            version: Some("2".into()),
            kind: "eusart".into(),
            register_version: "s2v2".into(),
            block: "EUSART".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "EUSART1_NS".into(),
            base_address: 0x5000_1000,
            version: Some("2".into()),
            kind: "eusart".into(),
            register_version: "s2v2_lf".into(),
            block: "EUSART".into(),
        });
        // A secure alias routed to a different version must not suppress the
        // version-neutral alias. Its instance constant uses the non-secure
        // peer's register type while retaining the secure SVD address.
        chip.peripherals.push(PeripheralInstance {
            name: "DMEM_NS".into(),
            base_address: 0x5000_2000,
            version: Some("2".into()),
            kind: "dmem".into(),
            register_version: "s2v2_fg25".into(),
            block: "DMEM".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "DMEM_S".into(),
            base_address: 0x4000_2000,
            version: Some("2".into()),
            kind: "dmem".into(),
            register_version: "s2v2".into(),
            block: "DMEM".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "SEMAILBOX_NS_HOST".into(),
            base_address: 0x5C00_0000,
            version: Some("1".into()),
            kind: "semailbox_ns_host".into(),
            register_version: "s2v1".into(),
            block: "SEMAILBOX_NS_HOST".into(),
        });
        // Secure `_S_` infix instance shares its NS peer's register type.
        chip.peripherals.push(PeripheralInstance {
            name: "SEMAILBOX_S_HOST".into(),
            base_address: 0x4C00_0000,
            version: Some("1".into()),
            kind: "semailbox_s_host".into(),
            register_version: "s2v1".into(),
            block: "SEMAILBOX_S_HOST".into(),
        });
        let s = build_chip_pac_rs(&chip, None);
        assert!(s.contains("pub use acmp_s2v2 as acmp;"), "missing acmp alias:\n{s}");
        assert!(s.contains("pub use dcdc_s2v1 as dcdc;"), "missing dcdc alias:\n{s}");
        assert!(
            s.contains("pub use dmem_s2v2_fg25 as dmem;"),
            "alias must follow the non-secure instance's version:\n{s}"
        );
        assert!(
            !s.contains(" as eusart;"),
            "eusart has two versions, must not be aliased:\n{s}"
        );
        assert!(
            !s.contains(" as semailbox_s_host;") && !s.contains("pub mod semailbox_s_host_s2v1;"),
            "secure infix register module must be deduplicated:\n{s}"
        );
        assert!(
            s.contains(
                "pub const SEMAILBOX_S_HOST: crate::semailbox_ns_host_s2v1::SemailboxNsHost = unsafe { crate::semailbox_ns_host_s2v1::SemailboxNsHost::from_ptr(0x4C000000 as *mut ()) };"
            ),
            "secure infix instance must retain its address with the NS type:\n{s}"
        );
    }

    #[test]
    fn metadata_rs_emits_per_kind_register_mod_decls() {
        let s = build_chip_metadata_rs(&fake_chip());
        assert!(
            s.contains("pub static METADATA: Metadata = Metadata {"),
            "missing METADATA static:\n{s}"
        );
        // Per-kind IR-static mod decls — declared inside `pub mod metadata`
        // so REGISTERS are reachable at `crate::metadata::<kind>_<version>`.
        assert!(
            s.contains("#[path = \"../../registers/acmp_s2v2.rs\"]\npub mod acmp_s2v2;"),
            "missing acmp_s2v2 register mod decl:\n{s}"
        );
        assert!(
            s.contains("#[path = \"../../registers/dcdc_s2v1.rs\"]\npub mod dcdc_s2v1;"),
            "missing dcdc_s2v1 register mod decl:\n{s}"
        );
        assert!(
            s.contains("Peripheral { name: \"ACMP0\", address: 0x5000E000, secure_address: Some(0x4000E000)"),
            "metadata must retain both TrustZone addresses:\n{s}"
        );
    }

    #[test]
    fn gpio_port_constants_follow_port_count() {
        let s = build_chip_pac_rs(&fake_chip(), Some(4));
        assert!(s.contains("pub mod gpio_port"), "missing gpio_port mod:\n{s}");
        assert!(s.contains("pub const PORTA: usize = 0;"));
        assert!(s.contains("pub const PORTD: usize = 3;"));
        assert!(!s.contains("PORTE"));

        // EFM32GG: GPIO_P_TypeDef P[6U] → ports A..F.
        let s = build_chip_pac_rs(&fake_chip(), Some(6));
        assert!(s.contains("pub const PORTF: usize = 5;"));

        let s = build_chip_pac_rs(&fake_chip(), None);
        assert!(!s.contains("gpio_port"));
    }

    #[test]
    fn gpio_port_count_reads_p_ctrl_array_len() {
        let yaml = "block/Gpio:\n  items:\n  - name: p_ctrl\n    array:\n      len: 6\n      stride: 36\n    byte_offset: 0\n    fieldset: regs::PortCtrl\n";
        let ir: chiptool::ir::IR = serde_yaml::from_str(yaml).unwrap();
        let mut chip = fake_chip();
        chip.peripherals.push(PeripheralInstance {
            name: "GPIO".into(),
            base_address: 0x4000_6000,
            version: None,
            kind: "gpio".into(),
            register_version: "s0v1".into(),
            block: "GPIO".into(),
        });
        let mut irs = BTreeMap::new();
        irs.insert(("gpio".to_string(), "s0v1".to_string()), ir);
        assert_eq!(gpio_port_count(&chip, &irs).unwrap(), Some(6));
        assert_eq!(gpio_port_count(&fake_chip(), &irs).unwrap(), None);
    }

    #[test]
    fn pac_rs_emits_nvic_prio_bits_from_chip() {
        let mut c = fake_chip();
        c.chip.nvic_prio_bits = Some(3);
        let s = build_chip_pac_rs(&c, None);
        assert!(s.contains("pub const NVIC_PRIO_BITS: u8 = 3;"), "{s}");
    }

    #[test]
    fn feature_name_lowercases() {
        assert_eq!(feature_name("EFR32MG26B211F2048IM68"), "efr32mg26b211f2048im68");
    }

    #[test]
    fn cargo_toml_uses_publish_template_and_appends_chip_features() {
        let s = render_cargo_toml(&["efr32mg24b210f1536im48".into(), "efr32mg26b211f2048im68".into()]);

        assert!(s.contains("version = \"0.6.0\""));
        assert!(s.contains("repository = \"https://github.com/andresv/silabs-data-generated\""));
        assert!(s.contains("[package.metadata.docs.rs]"));
        assert!(s.contains("\"build.rs\","));
        assert!(!s.contains("[workspace]"));
        assert!(s.ends_with("efr32mg24b210f1536im48 = []\nefr32mg26b211f2048im68 = []\n"));
    }

    #[test]
    fn series_literal_for_chip_dispatches_on_series() {
        // Series 2 — config fits in u8.
        let mut c = fake_chip();
        c.chip.series = Some(silabs_data_gen::header::Series { series: 2, config: 6 });
        assert_eq!(series_literal_for_chip(&c), "Series::Series2(6)");
        c.chip.series = Some(silabs_data_gen::header::Series { series: 2, config: 1 });
        assert_eq!(series_literal_for_chip(&c), "Series::Series2(1)");

        // Series 3 — config is u16 (3-digit numbering).
        c.chip.series = Some(silabs_data_gen::header::Series { series: 3, config: 301 });
        assert_eq!(series_literal_for_chip(&c), "Series::Series3(301)");

        // Series 0 — no config axis.
        c.chip.series = Some(silabs_data_gen::header::Series { series: 0, config: 0 });
        assert_eq!(series_literal_for_chip(&c), "Series::Series0");
        // Series 1 — config fits in u8.
        c.chip.series = Some(silabs_data_gen::header::Series { series: 1, config: 2 });
        assert_eq!(series_literal_for_chip(&c), "Series::Series1(2)");
    }

    #[test]
    #[should_panic(expected = "chip.series missing")]
    fn series_literal_for_chip_panics_on_unpopulated_series() {
        let mut c = fake_chip();
        c.chip.series = None;
        series_literal_for_chip(&c);
    }

    #[test]
    #[should_panic(expected = "unsupported chip series")]
    fn series_literal_for_chip_panics_on_unknown_series_number() {
        let mut c = fake_chip();
        c.chip.series = Some(silabs_data_gen::header::Series { series: 9, config: 1 });
        series_literal_for_chip(&c);
    }

    #[test]
    fn metadata_rs_emits_series_field() {
        let s = build_chip_metadata_rs(&fake_chip());
        assert!(
            s.contains("series: Series::Series2(6),"),
            "missing series field in metadata.rs:\n{s}"
        );
    }

    #[test]
    fn validates_series2_trustzone_address_bit() {
        let chip = fake_chip();
        validate_trustzone_aliases(&chip).unwrap();

        let mut bad = fake_chip();
        bad.peripherals
            .iter_mut()
            .find(|p| p.name == "ACMP0_S")
            .unwrap()
            .base_address = 0x5100_E000;
        let err = validate_trustzone_aliases(&bad).unwrap_err().to_string();
        assert!(err.contains("0x1000_0000"), "unexpected validation error: {err}");
    }
}
