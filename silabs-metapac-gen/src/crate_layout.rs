//! Assemble a `silabs-metapac` Cargo crate from per-chip JSON files.
//!
//! Layout produced:
//!
//! ```text
//! silabs-metapac/
//! ├── Cargo.toml
//! ├── README.md
//! └── src/
//!     ├── lib.rs
//!     ├── common.rs
//!     ├── peripherals/<kind>_<version>.rs
//!     └── chips/
//!         └── <chip>/
//!             ├── device.x      # cortex-m-rt linker fragment (or stub)
//!             └── mod.rs        # peripheral instances + interrupts + memory map
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use convert_case::{Boundary, Case, Casing};
use silabs_data_gen::chips::{ChipFile, Interrupt, PeripheralInstance};
use silabs_data_gen::pdsc::MemoryRegion;

use crate::pac::module_name;
use crate::peripheral::{nonsecure_to_secure_name, secure_to_nonsecure_name};

/// Convert a perimap-routed block name (e.g. `GPIO`, `EUSART`, `I2C`) into the
/// PascalCase identifier `chiptool::transform::sanitize::Sanitize::default()`
/// produces (e.g. `Gpio`, `Eusart`, `I2c`).
///
/// Mirrors chiptool's `sanitize_with_case`, which first removes digit
/// boundaries so `I2C` is treated as the merged token `i2c` rather than
/// three separate words. Without that step `I2C` would round-trip to `I2C`
/// under `Case::Pascal` and miss the struct named `I2c` in the YAML.
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

/// Validate the documented Series 2 address relationship. Exact addresses
/// still come from the SVD; the XOR is checked only as an invariant so a
/// changed vendor map fails generation instead of silently producing a
/// misleading API.
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
/// linker search path under the `rt` feature.
///
/// `cortex-m-rt`'s `link.x` does `INCLUDE device.x`, and `silabs-metapac`
/// emits a per-chip `device.x` into `src/chips/<chip>/`. Without this
/// helper the linker can't find it. Mirrors the analogous build script
/// in `stm32-metapac`.
pub fn write_build_rs(out: &Path) -> Result<()> {
    let s = r##"use std::env;
#[cfg(feature = "rt")]
use std::path::PathBuf;

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

fn main() {
    #[cfg(feature = "rt")]
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

    // Mirror stm32-metapac's env-var-driven include pattern: emit the
    // selected chip's pac.rs / metadata.rs paths so `lib.rs` can collapse
    // 66 cfg-gated `include!`s into a single `include!(env!(...))`.
    println!("cargo:rustc-env=SILABS_METAPAC_PAC_PATH=chips/{}/pac.rs", chip_name);
    println!(
        "cargo:rustc-env=SILABS_METAPAC_METADATA_PATH=chips/{}/metadata.rs",
        chip_name
    );

    println!("cargo:rerun-if-changed=build.rs");
}
"##;
    std::fs::write(out, s).with_context(|| format!("write build.rs at {}", out.display()))?;
    Ok(())
}

/// Write Cargo.toml with one boolean feature per chip OPN.
pub fn write_cargo_toml(chip_features: &[String], out: &Path) -> Result<()> {
    let mut s = String::new();
    s.push_str(
        r#"# Standalone crate — keep it out of any enclosing workspace.
[workspace]

[package]
name = "silabs-metapac"
version = "0.0.1"
edition = "2024"
license = "MIT OR Apache-2.0"
description = "Generated Silicon Labs PAC. Do not edit by hand — regenerate via silabs-metapac-gen."

[dependencies]
cortex-m = "0.7"
# `device` feature is required for the `cortex_m_rt::interrupt` proc-macro
# attribute referenced by the chiptool-emitted Interrupt enum.
cortex-m-rt = { version = "0.7", features = ["device"], optional = true }
defmt = { version = "0.3", optional = true }

[features]
default = ["pac"]

# Build the actual PAC. Set by default.
# If you just want the metadata, unset it with `default-features = false`.
pac = []

# Build the chip metadata.
# If set, a `silabs_metapac::metadata::METADATA` static will be exported,
# containing all the metadata for the currently selected chip.
metadata = []

# Implement the `defmt::Format` trait for many types.
defmt = ["dep:defmt"]

rt = ["cortex-m-rt"]

# Chip-selection features
"#,
    );
    for f in chip_features {
        s.push_str(&format!("{f} = []\n"));
    }
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

    // Mirrors stm32-metapac/res/src/lib.rs exactly — the heavy lifting
    // (per-kind `#[path] pub mod <kind>;` declarations and the typed
    // peripheral consts) is done inside `chips/<chip>/pac.rs` and
    // `chips/<chip>/metadata.rs`, which are selected by the env vars
    // emitted from `build.rs`. The `include!`d file's tokens carry their
    // original `Span`, so `#[path]` inside those files resolves relative
    // to the chip directory — not lib.rs.
    //
    // Chip-feature presence is enforced by `build.rs` (panics on zero or
    // multiple chip features) — matches stm32-data, which similarly has
    // no `compile_error!` in lib.rs.
    s.push_str("pub mod common;\n\n");

    s.push_str("#[cfg(feature = \"pac\")]\n");
    s.push_str("include!(env!(\"SILABS_METAPAC_PAC_PATH\"));\n\n");

    s.push_str("#[cfg(feature = \"metadata\")]\n");
    s.push_str("pub mod metadata {\n");
    s.push_str("    include!(\"metadata.rs\");\n");
    s.push_str("    include!(env!(\"SILABS_METAPAC_METADATA_PATH\"));\n");
    s.push_str("}\n");

    std::fs::write(out, s).with_context(|| format!("write lib.rs at {}", out.display()))?;
    Ok(())
}

/// Build the `chips/<chip>/mod.rs` content from a parsed ChipFile.
///
/// Each peripheral instance in `chip.peripherals` carries its routed
/// `(kind, register_version, block)` triple (assigned by perimap during
/// `silabs-data-gen gen`). We use those directly — there's no separate
/// `kinds` lookup parameter.
pub fn build_chip_pac_rs(chip: &ChipFile) -> String {
    let mut s = String::new();
    s.push_str("// Per-chip PAC content: peripheral module decls, typed peripheral\n");
    s.push_str("// consts, interrupt enum + cortex-m-rt glue, memory map.\n");
    s.push_str(&format!("// Generated for {}.\n//\n", chip.chip.name));
    s.push_str("// This file is `include!`d at the metapac crate root by `lib.rs`\n");
    s.push_str("// (selected via the `SILABS_METAPAC_PAC_PATH` env var emitted from\n");
    s.push_str("// `build.rs`). Mirrors `stm32-metapac`'s `chips/<chip>/pac.rs`\n");
    s.push_str("// layout — `#[path]` resolves relative to *this* file, so the\n");
    s.push_str("// `../../peripherals/...` paths below reach the shared chiptool\n");
    s.push_str("// peripheral modules under `src/peripherals/`.\n\n");

    // Per-kind chiptool peripheral mod decls. Each chip declares only
    // the (kind, version) pairs it actually uses.
    // Module names keep `<kind>_<version>` to support chips with multiple
    // versions of the same kind on the same die (e.g. EFR32MG26 with
    // `eusart_v2` + `eusart_v2_lf`).
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

        // Version-neutral aliases (`pub use cmu_v3 as cmu;`), emitted when the
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

    emit_gpio_port_constants(&mut s, &chip.peripherals);

    // Interrupts are emitted as the `pub enum Interrupt { … }` inside
    // `emit_cortex_m_rt_glue` — same shape as stm32-metapac. Numeric
    // values are reachable via `Interrupt::FOO as u16`.
    emit_cortex_m_rt_glue(&mut s, &chip.interrupts);

    s
}

fn emit_cortex_m_rt_glue(s: &mut String, interrupts: &[Interrupt]) {
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

    s.push_str("/// Number available in the NVIC for configuring priority.\n");
    s.push_str("#[cfg(feature = \"rt\")]\n");
    s.push_str("pub const NVIC_PRIO_BITS: u8 = 4;\n\n");

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
/// non-secure peer's register-block type but retains both exact SVD addresses.
/// For suffix pairs, the existing unsuffixed non-secure constant is retained
/// alongside the explicit `_S` constant; a redundant `_NS` constant is not
/// emitted.
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

fn emit_gpio_port_constants(s: &mut String, peripherals: &[PeripheralInstance]) {
    if !peripherals.iter().any(|p| p.name == "GPIO_NS" || p.name == "GPIO") {
        return;
    }
    s.push_str("/// GPIO port indices, mirroring `efr32mg<NN>_gpio.h`'s\n");
    s.push_str("/// `#define GPIO_PORTA 0` etc. Use as `GPIO.p(gpio_port::PORTC)`\n");
    s.push_str("/// (or just `GPIO.p(2)` — they're equivalent).\n");
    s.push_str("pub mod gpio_port {\n");
    for (i, ch) in ['A', 'B', 'C', 'D'].iter().enumerate() {
        s.push_str(&format!("    pub const PORT{ch}: usize = {i};\n"));
    }
    s.push_str("}\n\n");
}

/// Stub `device.x` placeholder.
pub fn stub_device_x(chip_name: &str) -> String {
    format!("/* device.x for {chip_name} not yet generated */\n")
}

/// Emit the `Series::Series<N>(<M>)` literal for a chip, sourced from
/// the `_SILICON_LABS_32B_SERIES_<N>_CONFIG_<M>` macros extracted from
/// the chip's CMSIS device header by `silabs-data-gen` (see
/// [`silabs_data_gen::header::extract_series`]).
///
/// Panics if the chip JSON predates the schema change that added
/// `Chip.series` — regenerate with `./d gen-all`. Also panics on a
/// series number outside the {2, 3} range, since the [`Series`] enum
/// only covers Series 2 + Series 3 today (matches the metapac side).
fn series_literal_for_chip(chip: &ChipFile) -> String {
    let s = chip
        .chip
        .series
        .expect("chip.series missing — re-run silabs-data-gen to populate it");
    match s.series {
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
/// Emits a `pub static METADATA: Metadata = …;` populated from the chip
/// JSON. Mirrors stm32-metapac's per-chip metadata module so HAL build
/// scripts can walk a chip's peripheral / interrupt / memory inventory
/// at build time.
///
/// The file is `include!`d into the metapac crate's top-level
/// `pub mod metadata` block (see [`write_lib_rs`]), so the type names
/// `Metadata`, `MemoryRegion`, `Peripheral`, `Interrupt` resolve against
/// the surrounding module without an explicit `use`.
///
/// Dedup: one metadata row owns each register layout. For a paired TrustZone
/// peripheral it carries both the non-secure and secure SVD base addresses.
pub fn build_chip_metadata_rs(chip: &ChipFile) -> String {
    let mut s = String::new();
    s.push_str("// Per-chip iterable metadata. Generated for ");
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

    // Per-kind IR-static mod decls. Each `<kind>_<version>.rs` exposes
    // `pub static REGISTERS: IR`. The
    // chip declares only the kinds it uses; `#[path]` is relative to this
    // file, so `../../registers/...` reaches `src/registers/`.
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
                    register_version: "v2".into(),
                    block: "ACMP".into(),
                },
                PeripheralInstance {
                    name: "ACMP0_S".into(),
                    base_address: 0x4000_E000,
                    version: Some("2".into()),
                    kind: "acmp".into(),
                    register_version: "v2".into(),
                    block: "ACMP".into(),
                },
                PeripheralInstance {
                    name: "DCDC".into(),
                    base_address: 0x4000_4000,
                    version: Some("1".into()),
                    kind: "dcdc".into(),
                    register_version: "v1".into(),
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
        let s = build_chip_pac_rs(&fake_chip());
        // The chip JSON's `block` field holds the perimap-routed name in raw
        // form (e.g. "ACMP"); `block_struct_ident` Pascal-cases it to match
        // `Sanitize::default()`'s output in the rendered register YAML.
        assert!(
            s.contains(
                "pub const ACMP0: crate::acmp_v2::Acmp = unsafe { crate::acmp_v2::Acmp::from_ptr(0x5000E000 as *mut ()) };"
            ),
            "missing typed ACMP0 const:\n{s}"
        );
        assert!(!s.contains("pub const ACMP0_NS:"), "redundant ACMP0_NS const:\n{s}");
        assert!(
            s.contains(
                "pub const ACMP0_S: crate::acmp_v2::Acmp = unsafe { crate::acmp_v2::Acmp::from_ptr(0x4000E000 as *mut ()) };"
            ),
            "missing secure ACMP0_S const:\n{s}"
        );
        assert!(
            s.contains("pub const DCDC: crate::dcdc_v1::Dcdc"),
            "missing typed DCDC const:\n{s}"
        );
        // Interrupts are emitted only as the `pub enum Interrupt` variants —
        // no separate `pub const ACMP0: u8 = 41;` const module, matching
        // stm32-metapac's pac.rs shape.
        assert!(s.contains("ACMP0 = 41,"), "missing ACMP0 enum variant:\n{s}");
        assert!(s.contains("TIMER0 = 25,"));
        assert!(!s.contains("pub const ACMP0: u8"));
        assert!(!s.contains("pub mod interrupts"));
        assert!(s.contains("IROM1_BASE: usize = 0x08000000"));
        assert!(s.contains("IROM1_SIZE: usize = 0x00200000"));

        // Per-kind chiptool mod decls — mirrors stm32 pac.rs structure.
        // Both acmp_v2 (used by ACMP0/1) and dcdc_v1 (used by DCDC) appear once.
        assert!(
            s.contains("#[path = \"../../peripherals/acmp_v2.rs\"]\npub mod acmp_v2;"),
            "missing acmp_v2 #[path] mod decl:\n{s}"
        );
        assert!(
            s.contains("#[path = \"../../peripherals/dcdc_v1.rs\"]\npub mod dcdc_v1;"),
            "missing dcdc_v1 #[path] mod decl:\n{s}"
        );
        assert_eq!(s.matches("pub mod acmp_v2;").count(), 1);
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
            register_version: "v2".into(),
            block: "EUSART".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "EUSART1_NS".into(),
            base_address: 0x5000_1000,
            version: Some("2".into()),
            kind: "eusart".into(),
            register_version: "v2_lf".into(),
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
            register_version: "v2_fg25".into(),
            block: "DMEM".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "DMEM_S".into(),
            base_address: 0x4000_2000,
            version: Some("2".into()),
            kind: "dmem".into(),
            register_version: "v2".into(),
            block: "DMEM".into(),
        });
        chip.peripherals.push(PeripheralInstance {
            name: "SEMAILBOX_NS_HOST".into(),
            base_address: 0x5C00_0000,
            version: Some("1".into()),
            kind: "semailbox_ns_host".into(),
            register_version: "v1".into(),
            block: "SEMAILBOX_NS_HOST".into(),
        });
        // Secure `_S_` infix instance shares its NS peer's register type.
        chip.peripherals.push(PeripheralInstance {
            name: "SEMAILBOX_S_HOST".into(),
            base_address: 0x4C00_0000,
            version: Some("1".into()),
            kind: "semailbox_s_host".into(),
            register_version: "v1".into(),
            block: "SEMAILBOX_S_HOST".into(),
        });
        let s = build_chip_pac_rs(&chip);
        assert!(s.contains("pub use acmp_v2 as acmp;"), "missing acmp alias:\n{s}");
        assert!(s.contains("pub use dcdc_v1 as dcdc;"), "missing dcdc alias:\n{s}");
        assert!(
            s.contains("pub use dmem_v2_fg25 as dmem;"),
            "alias must follow the non-secure instance's version:\n{s}"
        );
        assert!(
            !s.contains(" as eusart;"),
            "eusart has two versions, must not be aliased:\n{s}"
        );
        assert!(
            !s.contains(" as semailbox_s_host;") && !s.contains("pub mod semailbox_s_host_v1;"),
            "secure infix register module must be deduplicated:\n{s}"
        );
        assert!(
            s.contains(
                "pub const SEMAILBOX_S_HOST: crate::semailbox_ns_host_v1::SemailboxNsHost = unsafe { crate::semailbox_ns_host_v1::SemailboxNsHost::from_ptr(0x4C000000 as *mut ()) };"
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
            s.contains("#[path = \"../../registers/acmp_v2.rs\"]\npub mod acmp_v2;"),
            "missing acmp_v2 register mod decl:\n{s}"
        );
        assert!(
            s.contains("#[path = \"../../registers/dcdc_v1.rs\"]\npub mod dcdc_v1;"),
            "missing dcdc_v1 register mod decl:\n{s}"
        );
        assert!(
            s.contains("Peripheral { name: \"ACMP0\", address: 0x5000E000, secure_address: Some(0x4000E000)"),
            "metadata must retain both TrustZone addresses:\n{s}"
        );
    }

    #[test]
    fn gpio_port_constants_emitted_only_when_gpio_present() {
        let mut chip = fake_chip();
        chip.peripherals.push(PeripheralInstance {
            name: "GPIO_NS".into(),
            base_address: 0x5003_C000,
            version: Some("7".into()),
            kind: "gpio".into(),
            register_version: "v7".into(),
            block: "GPIO".into(),
        });
        let s = build_chip_pac_rs(&chip);
        assert!(s.contains("pub mod gpio_port"), "missing gpio_port mod:\n{s}");
        assert!(s.contains("pub const PORTA: usize = 0;"));
        assert!(s.contains("pub const PORTD: usize = 3;"));

        let s = build_chip_pac_rs(&fake_chip());
        assert!(!s.contains("gpio_port"));
    }

    #[test]
    fn feature_name_lowercases() {
        assert_eq!(feature_name("EFR32MG26B211F2048IM68"), "efr32mg26b211f2048im68");
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
