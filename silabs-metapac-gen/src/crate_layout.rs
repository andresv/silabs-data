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

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use convert_case::{Boundary, Case, Casing};
use silabs_data_gen::chips::{ChipFile, Interrupt, PeripheralInstance};
use silabs_data_gen::pdsc::MemoryRegion;

use crate::pac::module_name;

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

    // ----- Per-kind chiptool peripheral mod decls -----
    // Each chip declares only the (kind, version) pairs it actually uses.
    // Module names keep `<kind>_<version>` to support chips with multiple
    // versions of the same kind on the same die (e.g. EFR32MG26 with
    // `eusart_v2` + `eusart_v2_lf`).
    let mut kinds: BTreeSet<(String, String)> = BTreeSet::new();
    for p in &chip.peripherals {
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
    }

    s.push_str("/// Memory map (flash/RAM regions, from the CMSIS pdsc).\n");
    s.push_str("pub mod memory {\n");
    for m in &chip.chip.memory {
        emit_memory_consts(&mut s, m);
    }
    s.push_str("}\n\n");

    s.push_str("/// Typed peripheral instance constants.\n");
    s.push_str("///\n");
    s.push_str("/// Each peripheral is exposed once at its **non-secure** address (the alias\n");
    s.push_str("/// reachable from non-secure CPU state on TrustZone-enabled images). The\n");
    s.push_str("/// secure alias for any peripheral on Series 2 is `addr ^ 0x0100_0000`.\n");
    s.push_str("/// Secure-state code can XOR the bit explicitly when crossing the\n");
    s.push_str("/// security boundary.\n");
    emit_typed_peripheral_consts(&mut s, &chip.peripherals);

    emit_gpio_port_constants(&mut s, &chip.peripherals);

    s.push_str("/// Cortex-M interrupt numbers (deduped by name).\n");
    s.push_str("pub mod interrupts {\n");
    emit_interrupt_consts(&mut s, &chip.interrupts);
    s.push_str("}\n\n");

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

/// Emit typed peripheral instance consts, one per NS peripheral. Each carries
/// its perimap-routed `(kind, register_version, block)` triple in the chip
/// JSON; we reference the resulting `crate::<kind>_<version>::<Block>` type.
fn emit_typed_peripheral_consts(s: &mut String, peripherals: &[PeripheralInstance]) {
    use std::collections::BTreeMap;
    let mut by_base: BTreeMap<String, &PeripheralInstance> = BTreeMap::new();
    for p in peripherals {
        if p.name.ends_with("_S") && !p.name.ends_with("_NS") {
            continue;
        }
        let base_name = p
            .name
            .strip_suffix("_NS")
            .map(str::to_owned)
            .unwrap_or_else(|| p.name.clone());
        by_base.entry(base_name).or_insert(p);
    }

    for (name, p) in by_base {
        let mod_name = module_name(&p.kind, &p.register_version);
        let struct_name = block_struct_ident(&p.block);
        s.push_str(&format!(
            "pub const {name}: crate::{mod_name}::{struct_name} = unsafe {{ \
             crate::{mod_name}::{struct_name}::from_ptr(0x{:08X} as *mut ()) }};\n",
            p.base_address
        ));
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

fn emit_interrupt_consts(s: &mut String, interrupts: &[Interrupt]) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for i in interrupts {
        if !seen.insert(i.name.clone()) {
            continue;
        }
        if let Some(desc) = &i.description {
            let desc = desc.replace('\n', " ").replace('\r', "");
            s.push_str(&format!("    /// {desc}\n"));
        }
        s.push_str(&format!(
            "    pub const {name}: u8 = {value};\n",
            name = i.name,
            value = i.value
        ));
    }
}

/// Stub `device.x` placeholder.
pub fn stub_device_x(chip_name: &str) -> String {
    format!("/* device.x for {chip_name} not yet generated */\n")
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
/// Dedup: only the non-secure alias of each peripheral is emitted (same
/// rule [`emit_typed_peripheral_consts`] applies for the typed consts).
pub fn build_chip_metadata_rs(chip: &ChipFile) -> String {
    use std::collections::BTreeMap;

    let mut s = String::new();
    s.push_str("// Per-chip iterable metadata. Generated for ");
    s.push_str(&chip.chip.name);
    s.push_str(".\n//\n");
    s.push_str("// Included from `pub mod metadata` in the metapac crate root;\n");
    s.push_str("// type names resolve to the surrounding module — see\n");
    s.push_str("// silabs-metapac-gen/res/metadata.rs.\n\n");

    // ----- Peripherals: dedup NS/S, strip the `_NS` suffix. -----
    let mut by_base: BTreeMap<String, &PeripheralInstance> = BTreeMap::new();
    for p in &chip.peripherals {
        if p.name.ends_with("_S") && !p.name.ends_with("_NS") {
            continue;
        }
        let base_name = p
            .name
            .strip_suffix("_NS")
            .map(str::to_owned)
            .unwrap_or_else(|| p.name.clone());
        by_base.entry(base_name).or_insert(p);
    }

    // ----- Interrupts: dedup by name, preserve value ordering. -----
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

    s.push_str("    memory: &[\n");
    for m in &chip.chip.memory {
        s.push_str(&format!(
            "        MemoryRegion {{ name: {:?}, address: 0x{:08X}, size: 0x{:08X}, access: {:?} }},\n",
            m.id, m.start, m.size, m.access,
        ));
    }
    s.push_str("    ],\n");

    s.push_str("    peripherals: &[\n");
    for (name, p) in &by_base {
        s.push_str(&format!(
            "        Peripheral {{ name: {:?}, address: 0x{:08X}, kind: {:?}, version: {:?}, block: {:?} }},\n",
            name, p.base_address, p.kind, p.register_version, p.block,
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

    // ----- Per-kind IR-static mod decls -----
    // Each `<kind>_<version>.rs` exposes `pub static REGISTERS: IR`. The
    // chip declares only the kinds it uses; `#[path]` is relative to this
    // file, so `../../registers/...` reaches `src/registers/`.
    let mut kinds: BTreeSet<(String, String)> = BTreeSet::new();
    for p in &chip.peripherals {
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
                    base_address: 0x4000_E000,
                    version: Some("2".into()),
                    kind: "acmp".into(),
                    register_version: "v2".into(),
                    block: "ACMP".into(),
                },
                PeripheralInstance {
                    name: "ACMP0_S".into(),
                    base_address: 0x5000_E000,
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
                "pub const ACMP0: crate::acmp_v2::Acmp = unsafe { crate::acmp_v2::Acmp::from_ptr(0x4000E000 as *mut ()) };"
            ),
            "missing typed ACMP0 const:\n{s}"
        );
        assert!(!s.contains("ACMP0_S"), "_S consts must not be emitted; got:\n{s}");
        assert!(
            s.contains("pub const DCDC: crate::dcdc_v1::Dcdc"),
            "missing typed DCDC const:\n{s}"
        );
        assert_eq!(s.matches("pub const ACMP0: u8").count(), 1);
        assert!(s.contains("pub const TIMER0: u8 = 25"));
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
}
