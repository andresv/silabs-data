//! Expand EFR32 Series 2 SET/CLR/TGL register aliases.
//!
//! On EFR32 Series 2, register-banked peripherals expose the entire base
//! register layout three more times at fixed offsets from the peripheral
//! base address:
//!
//! - `+0x1000`: SET alias — writing `1` bits sets those bits.
//! - `+0x2000`: CLR alias — writing `1` bits clears those bits.
//! - `+0x3000`: TGL alias — writing `1` bits toggles those bits.
//!
//! The vendor SVD describes only the base layout. The CMSIS device header
//! marks each banked peripheral with `#define <PERI>_HAS_SET_CLEAR`.
//!
//! Only writable registers get aliases. The alias of a read-only register
//! reads the base value and ignores writes, so it is of no use.
//!
//! Series 2 ignores a write to the base `IF` register. A flag clears only
//! through `IF_CLR`.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use chiptool::ir::{Access, BlockItem, BlockItemInner, IR};
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

use crate::pac::IpKey;

/// Lowercase kinds that the extracted packs mark with
/// `#define <PERI>_HAS_SET_CLEAR`. Empty when `extract_dirs` is empty.
///
/// Reads every `Device/SiliconLabs/<FAMILY>/Include/*.h`. The marker is in
/// the per-peripheral headers, which the per-chip header includes.
pub fn discover_banked_kinds(extract_dirs: &[&Path]) -> Result<HashSet<String>> {
    static MARKER_RE: OnceLock<Regex> = OnceLock::new();
    let marker = MARKER_RE.get_or_init(|| {
        // Anchored: must be a real `#define` directive, not an in-comment mention.
        Regex::new(r"(?m)^\s*#\s*define\s+([A-Z][A-Z0-9_]*)_HAS_SET_CLEAR\b").expect("regex compiles")
    });

    let mut out: HashSet<String> = HashSet::new();
    for dir in extract_dirs {
        let include_glob = dir.join("Device/SiliconLabs");
        if !include_glob.is_dir() {
            continue;
        }
        // <extract>/Device/SiliconLabs/<FAMILY>/Include/*.h
        for family in std::fs::read_dir(&include_glob).with_context(|| format!("read {}", include_glob.display()))? {
            let family = family?;
            let include = family.path().join("Include");
            if !include.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&include).with_context(|| format!("read {}", include.display()))? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("h") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
                for caps in marker.captures_iter(&text) {
                    out.insert(caps.get(1).unwrap().as_str().to_ascii_lowercase());
                }
            }
        }
    }
    Ok(out)
}

/// The `(kind, version)` modules that get SET/CLR/TGL aliases: a kind in
/// `banked_kinds` that at least one Series 2+ chip uses.
///
/// `banked_kinds` holds bare kind names from every pack. Without the series
/// check, a Series 0/1 block of the same kind (`letimer_s0v1`) would get
/// aliases that its hardware does not have.
pub fn banked_keys(chips: &[ChipFile], banked_kinds: &HashSet<String>) -> BTreeSet<IpKey> {
    chips
        .iter()
        .filter(|c| c.chip.series.is_some_and(|s| s.series >= 2))
        .flat_map(|c| &c.peripherals)
        .filter(|p| banked_kinds.contains(&p.kind))
        .map(|p| (p.kind.clone(), p.register_version.clone()))
        .collect()
}

const SET_OFFSET: u32 = 0x1000;
const CLR_OFFSET: u32 = 0x2000;
const TGL_OFFSET: u32 = 0x3000;

/// For each writable register in every block of `ir`, append three
/// write-only aliases (`<name>_set`, `<name>_clr`, `<name>_tgl`) at
/// `byte_offset + 0x1000 / 0x2000 / 0x3000`. The fieldset is reused, so
/// the aliases share the bit-field definitions with the base register.
///
/// The aliases go at the end of the block. Chiptool output does not depend
/// on the item order.
pub fn expand_series2_aliases(ir: &mut IR) {
    for block in ir.blocks.values_mut() {
        let mut aliases: Vec<BlockItem> = Vec::new();
        for item in &block.items {
            let BlockItemInner::Register(reg) = &item.inner else {
                // Every register YAML has flat blocks. A nested block needs
                // its own aliases, so stop instead of emitting wrong code.
                panic!(
                    "expand_series2_aliases: unexpected nested block item `{}` — \
                     extend this transform to handle BlockItemBlock items",
                    item.name,
                );
            };
            if reg.access == Access::Read {
                continue;
            }
            // Strip chiptool's trailing `_` keyword escape (`if_`), so the
            // alias is `if_clr`, not `if__clr`. No suffixed name is a keyword.
            let base_name = item.name.strip_suffix('_').unwrap_or(&item.name);
            for (suffix, offset) in [("set", SET_OFFSET), ("clr", CLR_OFFSET), ("tgl", TGL_OFFSET)] {
                let alias_reg = chiptool::ir::Register {
                    access: Access::Write,
                    bit_size: reg.bit_size,
                    fieldset: reg.fieldset.clone(),
                };
                aliases.push(BlockItem {
                    name: format!("{base_name}_{suffix}"),
                    description: item
                        .description
                        .as_ref()
                        .map(|d| format!("{d} (write-1-to-{suffix} alias)")),
                    array: item.array.clone(),
                    byte_offset: item.byte_offset + offset,
                    inner: BlockItemInner::Register(alias_reg),
                });
            }
        }
        block.items.extend(aliases);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chiptool::ir::{Block, BlockItem, BlockItemInner, IR, Register};

    use super::*;

    fn chip_with(name: &str, series: u8, kind: &str, version: &str) -> ChipFile {
        ChipFile {
            chip: silabs_data_gen::pdsc::Chip {
                name: name.into(),
                core: "Cortex-M33".into(),
                fpu: false,
                mpu: false,
                trustzone: false,
                series: Some(silabs_data_gen::header::Series { series, config: 0 }),
                nvic_prio_bits: Some(4),
                memory: vec![],
                flash_algo: None,
                svd: "x.svd".into(),
                package: None,
            },
            peripherals: vec![silabs_data_gen::chips::PeripheralInstance {
                name: "LETIMER0".into(),
                base_address: 0x4000_0000,
                version: None,
                kind: kind.into(),
                register_version: version.into(),
                block: "LETIMER".into(),
            }],
            interrupts: vec![],
        }
    }

    /// `_HAS_SET_CLEAR` is discovered per kind name across all packs, but
    /// SET/CLR/TGL alias regions only exist on Series 2+. A Series 0 block
    /// of the same kind (`letimer_s0v1`) must not get aliases.
    #[test]
    fn banked_keys_only_cover_series_2_and_later() {
        let chips = vec![
            chip_with("EFR32MG24B210F1536IM48", 2, "letimer", "s2v1"),
            chip_with("EFM32GG390F1024", 0, "letimer", "s0v1"),
        ];
        let kinds: HashSet<String> = ["letimer".to_string()].into();
        let keys = banked_keys(&chips, &kinds);
        assert!(keys.contains(&("letimer".to_string(), "s2v1".to_string())));
        assert!(!keys.contains(&("letimer".to_string(), "s0v1".to_string())));
    }

    fn rw_reg(name: &str, offset: u32, fieldset: &str) -> BlockItem {
        BlockItem {
            name: name.to_string(),
            description: Some(format!("{name} register")),
            array: None,
            byte_offset: offset,
            inner: BlockItemInner::Register(Register {
                access: Access::ReadWrite,
                bit_size: 32,
                fieldset: Some(fieldset.to_string()),
            }),
        }
    }

    fn ro_reg(name: &str, offset: u32) -> BlockItem {
        BlockItem {
            name: name.to_string(),
            description: None,
            array: None,
            byte_offset: offset,
            inner: BlockItemInner::Register(Register {
                access: Access::Read,
                bit_size: 32,
                fieldset: None,
            }),
        }
    }

    fn make_ir(items: Vec<BlockItem>) -> IR {
        let mut blocks = BTreeMap::new();
        blocks.insert(
            "Timer".to_string(),
            Block {
                extends: None,
                description: None,
                items,
            },
        );
        IR {
            blocks,
            fieldsets: BTreeMap::new(),
            enums: BTreeMap::new(),
            devices: BTreeMap::new(),
        }
    }

    #[test]
    fn writable_registers_gain_three_aliases_at_fixed_offsets() {
        // Use `if_` to also exercise the Rust-keyword escape stripping:
        // the alias must be `if_clr`, not `if__clr`.
        let mut ir = make_ir(vec![rw_reg("if_", 0x14, "regs::If")]);
        expand_series2_aliases(&mut ir);

        let items = &ir.blocks["Timer"].items;
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["if_", "if_set", "if_clr", "if_tgl"]);

        // Check offsets and access on each alias.
        let by_name: std::collections::HashMap<&str, &BlockItem> = items.iter().map(|i| (i.name.as_str(), i)).collect();
        assert_eq!(by_name["if_set"].byte_offset, 0x14 + 0x1000);
        assert_eq!(by_name["if_clr"].byte_offset, 0x14 + 0x2000);
        assert_eq!(by_name["if_tgl"].byte_offset, 0x14 + 0x3000);
        for suffix in ["_set", "_clr", "_tgl"] {
            let alias = by_name[format!("if{suffix}").as_str()];
            let BlockItemInner::Register(r) = &alias.inner else {
                panic!("alias not a Register");
            };
            assert_eq!(r.access, Access::Write);
            assert_eq!(r.fieldset.as_deref(), Some("regs::If"));
            assert_eq!(r.bit_size, 32);
        }
    }

    #[test]
    fn read_only_registers_get_no_aliases() {
        let mut ir = make_ir(vec![ro_reg("ipversion", 0x00), rw_reg("ctrl", 0x08, "regs::Ctrl")]);
        expand_series2_aliases(&mut ir);

        let names: Vec<&str> = ir.blocks["Timer"].items.iter().map(|i| i.name.as_str()).collect();
        // ctrl gets 3 aliases; ipversion gets none.
        assert!(names.contains(&"ipversion"));
        assert!(!names.iter().any(|n| n.starts_with("ipversion_")));
        for suffix in ["_set", "_clr", "_tgl"] {
            assert!(
                names.contains(&format!("ctrl{suffix}").as_str()),
                "missing ctrl{suffix} in {names:?}"
            );
        }
    }

    #[test]
    fn arrays_propagate_to_aliases() {
        let mut item = rw_reg("p_dout", 0x40, "regs::PortDout");
        item.array = Some(chiptool::ir::Array::Regular(chiptool::ir::RegularArray {
            len: 4,
            stride: 48,
        }));
        let mut ir = make_ir(vec![item]);
        expand_series2_aliases(&mut ir);

        let by_name: std::collections::HashMap<String, &BlockItem> =
            ir.blocks["Timer"].items.iter().map(|i| (i.name.clone(), i)).collect();
        for suffix in ["set", "clr", "tgl"] {
            let alias = &by_name[&format!("p_dout_{suffix}")];
            assert!(
                matches!(
                    alias.array,
                    Some(chiptool::ir::Array::Regular(chiptool::ir::RegularArray {
                        len: 4,
                        stride: 48
                    }))
                ),
                "{suffix} alias lost the array attribute"
            );
        }
    }

    /// Uses a temporary `Device/SiliconLabs/<FAMILY>/Include` tree, so the
    /// test does not need `silabs-data-source/`.
    #[test]
    fn discover_banked_kinds_picks_up_has_set_clear_defines() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("expand-aliases-test-{}", std::process::id()));
        let include = tmp.join("Device/SiliconLabs/EFR32XX/Include");
        fs::create_dir_all(&include).unwrap();

        fs::write(
            include.join("efr32xx_timer.h"),
            "/* ... */\n#define TIMER_HAS_SET_CLEAR\n#define TIMER_FOO 1\n",
        )
        .unwrap();
        fs::write(include.join("efr32xx_gpio.h"), "#define GPIO_HAS_SET_CLEAR\n").unwrap();
        // Non-banked peripheral — no marker.
        fs::write(include.join("efr32xx_aes.h"), "#define AES_FOO 1\n").unwrap();
        // In-comment mention must NOT match.
        fs::write(
            include.join("efr32xx_buzz.h"),
            "/* This file does NOT have BUZZ_HAS_SET_CLEAR. */\n",
        )
        .unwrap();

        let found = discover_banked_kinds(&[tmp.as_path()]).expect("discover");
        assert!(found.contains("timer"), "found: {found:?}");
        assert!(found.contains("gpio"), "found: {found:?}");
        assert!(!found.contains("aes"), "found: {found:?}");
        assert!(!found.contains("buzz"), "found: {found:?}");

        // Output is lowercase.
        assert!(!found.contains("TIMER"));

        fs::remove_dir_all(&tmp).ok();
    }
}
