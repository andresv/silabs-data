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
//! The vendor SVD does not enumerate these aliases (only the base layout
//! is described). The per-chip CMSIS device header marks each banked
//! peripheral with `#define <PERI>_HAS_SET_CLEAR` and declares the
//! aliases as full struct mirrors with `_SET`/`_CLR`/`_TGL` suffixes.
//!
//! This transform replicates the base block's writable registers as
//! write-only aliases at the three offsets. Read-only registers
//! (`access: Read`) are skipped — their aliases exist at the hardware
//! level but reads return the base value and writes are no-ops, so
//! generating Rust accessors for them adds no value.
//!
//! The most common consumer of these aliases is the `IF` (interrupt
//! flag) register on Series 2: writes to base `IF` are *silently
//! ignored*, and clearing flags requires writing to `IF_CLR`.

use chiptool::ir::{Access, BlockItem, BlockItemInner, IR};

/// Peripheral kinds with `_HAS_SET_CLEAR` on EFR32MG24/MG26. Derived by
/// grepping the CMSIS device headers in `Device/SiliconLabs/<FAMILY>/
/// Include/` for `_HAS_SET_CLEAR`. Lowercase, matches `kind` in
/// `data/registers/<kind>_v<version>.yaml`.
const BANKED_KINDS: &[&str] = &[
    "acmp", "amuxcp", "buram", "burtc", "cmu", "dcdc", "dpll", "emu",
    "eusart", "fsrco", "gpcrc", "gpio", "hfrco", "hfxo", "i2c", "iadc",
    "icache", "keyscan", "lcd", "lcdrf", "ldma", "ldmaxbar", "letimer",
    "lfrco", "lfxo", "mailbox", "mpahbram", "msc", "mvp", "pcnt", "prs",
    "smu", "syscfg", "sysrtc", "timer", "ulfrco", "usart", "vdac", "wdog",
];

/// `true` if the given lowercase peripheral kind is register-banked on
/// EFR32 Series 2 (`#define <PERI>_HAS_SET_CLEAR` in the device header).
pub fn is_banked(kind: &str) -> bool {
    BANKED_KINDS.contains(&kind)
}

const SET_OFFSET: u32 = 0x1000;
const CLR_OFFSET: u32 = 0x2000;
const TGL_OFFSET: u32 = 0x3000;

/// For each writable register in every block of `ir`, append three
/// write-only aliases (`<name>_set`, `<name>_clr`, `<name>_tgl`) at
/// `byte_offset + 0x1000 / 0x2000 / 0x3000`. The fieldset is reused, so
/// the aliases share the bit-field definitions with the base register.
///
/// Items are appended at the end of the block — chiptool's render is
/// order-insensitive for `BlockItem` lists.
pub fn expand_series2_aliases(ir: &mut IR) {
    for block in ir.blocks.values_mut() {
        let mut aliases: Vec<BlockItem> = Vec::new();
        for item in &block.items {
            let BlockItemInner::Register(reg) = &item.inner else {
                // Sub-block references aren't expected in the current
                // Silabs register YAMLs (every kind YAML is a flat list
                // of Register items). If one shows up later, the right
                // thing is to expand the inner block recursively at the
                // alias offset — but that's not needed today, so panic
                // loudly to flag the assumption rather than emit silently
                // wrong code.
                panic!(
                    "expand_series2_aliases: unexpected nested block item `{}` — \
                     extend this transform to handle BlockItemBlock items",
                    item.name,
                );
            };
            if reg.access == Access::Read {
                continue;
            }
            // Strip a trailing `_` from the base name (chiptool's
            // Sanitize escape for Rust keywords like `if`, `loop`) so
            // we get `if_clr` and not `if__clr`. Adding `_set`/`_clr`/
            // `_tgl` never produces a keyword, so no re-escape needed.
            let base_name = item.name.strip_suffix('_').unwrap_or(&item.name);
            for (suffix, offset) in
                [("set", SET_OFFSET), ("clr", CLR_OFFSET), ("tgl", TGL_OFFSET)]
            {
                let alias_reg = chiptool::ir::Register {
                    access: Access::Write,
                    bit_size: reg.bit_size,
                    fieldset: reg.fieldset.clone(),
                };
                aliases.push(BlockItem {
                    name: format!("{base_name}_{suffix}"),
                    description: item.description.as_ref().map(|d| {
                        format!("{d} (write-1-to-{suffix} alias)")
                    }),
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
    use super::*;
    use chiptool::ir::{Block, BlockItem, BlockItemInner, IR, Register};
    use std::collections::BTreeMap;

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
        let by_name: std::collections::HashMap<&str, &BlockItem> =
            items.iter().map(|i| (i.name.as_str(), i)).collect();
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
        let mut ir = make_ir(vec![
            ro_reg("ipversion", 0x00),
            rw_reg("ctrl", 0x08, "regs::Ctrl"),
        ]);
        expand_series2_aliases(&mut ir);

        let names: Vec<&str> = ir.blocks["Timer"]
            .items
            .iter()
            .map(|i| i.name.as_str())
            .collect();
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

        let by_name: std::collections::HashMap<String, &BlockItem> = ir.blocks["Timer"]
            .items
            .iter()
            .map(|i| (i.name.clone(), i))
            .collect();
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

    #[test]
    fn banked_kind_membership() {
        assert!(is_banked("timer"));
        assert!(is_banked("gpio"));
        assert!(is_banked("eusart"));
        assert!(!is_banked("aes"));
        assert!(!is_banked("semailbox"));
        assert!(!is_banked("TIMER"), "kind list is case-sensitive lowercase");
    }
}
