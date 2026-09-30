//! Per-peripheral clock-enable bits, like stm32-data's `rcc.enable`.
//!
//! Every CMU clock-gate field is named after the peripheral it gates:
//! Series 2 `CLKENn.TIMER4`, Series 0 `HFPERCLKEN0.TIMER0` /
//! `LFACLKEN0.LETIMER0`. So a peripheral's enable bit is found by matching
//! its instance name against the fields of the chip's own CMU gate
//! registers. [`OVERRIDES`] covers the few names that don't match.
//! Peripherals without a gate (CMU, EMU, DEVINFO, ...) are always clocked
//! and get `None`.
//!
//! The metapac resolves the register address and bit here, so a consumer
//! can use them in a `const` lookup without walking the register IR.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chiptool::ir::{BitOffset, BlockItemInner, IR};
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

use crate::pac::IpKey;

/// One clock-gate bit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockEnable {
    /// CMU register name as in the curated YAML (`clken1`).
    pub register: String,
    /// Field name in that register (`timer4`).
    pub field: String,
    /// Absolute address of the non-secure CMU register.
    pub address: u64,
    /// Bit number of the field.
    pub bit: u32,
}

/// CMU registers that hold clock gates: `CLKENn` on Series 1+ and the
/// Series 0 bus-specific enable registers.
const GATE_REGISTERS: &str = r"^(clken\d+|hfcoreclken0|hfperclken0|lfaclken0|lfbclken0)$";

/// Peripherals whose gate field has a different name, as
/// `(peripheral, field)`. DMEM0 and DMEM1 share one gate on MG26, and the
/// CRYPTOACC register views share the CRYPTOACC gate.
const OVERRIDES: &[(&str, &str)] = &[
    ("CRYPTOACC_NS_PKCTRL", "cryptoacc"),
    ("CRYPTOACC_NS_RNGCTRL", "cryptoacc"),
    ("DMEM0", "dmem"),
    ("DMEM1", "dmem"),
    ("SEMAILBOX_NS_HOST", "semailboxhost"),
    ("USB_NS_APBS", "usb"),
    ("USBAHB_NS_AHBS", "usb"),
];

/// Clock-enable bit per canonical peripheral name for one chip.
pub fn clock_enables(
    chip: &ChipFile,
    names: &[String],
    irs: &BTreeMap<IpKey, IR>,
) -> Result<BTreeMap<String, ClockEnable>> {
    let cmu = chip
        .peripherals
        .iter()
        .find(|p| p.kind == "cmu" && !p.name.ends_with("_S"))
        .with_context(|| format!("{} has no CMU", chip.chip.name))?;
    let ir = irs
        .get(&(cmu.kind.clone(), cmu.register_version.clone()))
        .with_context(|| format!("no IR for cmu_{}", cmu.register_version))?;
    let block = ir
        .blocks
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&cmu.block))
        .map(|(_, b)| b)
        .context("CMU IR has no block")?;

    let gate_re = Regex::new(GATE_REGISTERS).expect("GATE_REGISTERS compiles");
    // field name → gate, across every gate register.
    let mut gates: BTreeMap<String, ClockEnable> = BTreeMap::new();
    for item in &block.items {
        if !gate_re.is_match(&item.name) || item.array.is_some() {
            continue;
        }
        let BlockItemInner::Register(reg) = &item.inner else {
            continue;
        };
        let Some(fs_name) = &reg.fieldset else { continue };
        let fs = ir
            .fieldsets
            .get(fs_name)
            .with_context(|| format!("CMU fieldset {fs_name} missing"))?;
        for f in &fs.fields {
            let BitOffset::Regular(bit) = f.bit_offset else {
                bail!("CMU {}.{} has a split bit offset", item.name, f.name);
            };
            if f.bit_size != 1 {
                continue;
            }
            let gate = ClockEnable {
                register: item.name.clone(),
                field: f.name.clone(),
                address: cmu.base_address + u64::from(item.byte_offset),
                bit,
            };
            if let Some(prev) = gates.insert(f.name.clone(), gate) {
                bail!("CMU field `{}` is in both {} and {}", f.name, prev.register, item.name);
            }
        }
    }

    let mut out = BTreeMap::new();
    for canonical in names {
        let field = OVERRIDES
            .iter()
            .find(|(p, _)| p == canonical)
            .map(|(_, f)| (*f).to_owned())
            .unwrap_or_else(|| canonical.to_ascii_lowercase());
        if let Some(gate) = gates.get(&field) {
            out.insert(canonical.clone(), gate.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmu_ir() -> IR {
        serde_yaml::from_str(
            "
block/Cmu:
  items:
  - name: clken0
    byte_offset: 100
    fieldset: regs::Clken0
  - name: clken1
    byte_offset: 104
    fieldset: regs::Clken1
  - name: clken0_set
    byte_offset: 4196
    access: Write
    fieldset: regs::Clken0
fieldset/regs::Clken0:
  fields:
  - name: timer0
    bit_offset: 5
    bit_size: 1
fieldset/regs::Clken1:
  fields:
  - name: timer4
    bit_offset: 31
    bit_size: 1
  - name: dmem
    bit_offset: 2
    bit_size: 1
",
        )
        .unwrap()
    }

    fn chip() -> ChipFile {
        serde_json::from_value(serde_json::json!({
            "chip": { "name": "FAKE", "core": "CM33", "fpu": true, "mpu": true, "trustzone": true,
                      "memory": [], "svd": "x.svd" },
            "peripherals": [
                { "name": "CMU_NS", "base_address": 0x5000_8000u64, "version": "3",
                  "kind": "cmu", "register_version": "s2v3", "block": "Cmu" }
            ],
            "interrupts": []
        }))
        .unwrap()
    }

    #[test]
    fn matches_by_name_and_override_and_skips_aliases() {
        let mut irs = BTreeMap::new();
        irs.insert(("cmu".to_owned(), "s2v3".to_owned()), cmu_ir());
        let names = ["TIMER0", "TIMER4", "DMEM1", "EMU"].map(String::from);
        let got = clock_enables(&chip(), &names, &irs).unwrap();
        assert_eq!(
            got["TIMER4"],
            ClockEnable {
                register: "clken1".into(),
                field: "timer4".into(),
                address: 0x5000_8068,
                bit: 31
            }
        );
        assert_eq!(got["TIMER0"].register, "clken0", "must not pick the clken0_set alias");
        assert_eq!(got["DMEM1"].field, "dmem");
        assert!(!got.contains_key("EMU"));
    }
}
