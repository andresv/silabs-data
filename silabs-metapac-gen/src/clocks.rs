//! Per-peripheral CMU data: the clock-gate bit (`enable`) and the clock
//! that drives the peripheral (`kernel_clock`).
//!
//! # Clock gates
//!
//! Every CMU clock-gate field is named after the peripheral it gates:
//! Series 2 `CLKENn.TIMER4`, Series 0 `HFPERCLKEN0.TIMER0` /
//! `LFACLKEN0.LETIMER0` / `PCNTCTRL.PCNT0CLKEN` (the `CLKEN` suffix is
//! dropped). So a peripheral's enable bit is found by matching
//! its instance name against the fields of the chip's own CMU gate
//! registers. [`OVERRIDES`] covers the few names that don't match.
//! Peripherals without a gate (CMU, EMU, DEVINFO, ...) are always clocked
//! and get `None`.
//!
//! The metapac resolves the register address and bit here, so a consumer
//! can use them in a `const` lookup without walking the register IR.
//!
//! # Kernel clocks
//!
//! In order:
//!
//! 1. Mux. The peripheral has its own clock select in the CMU, found by
//!    name: Series 2 `<name>CLKCTRL.CLKSEL` (`EUSART0CLKCTRL`), Series 0
//!    `<name>CLKSEL` (`PCNTCTRL.PCNT0CLKSEL`). Each variant of the field's
//!    enum names a source clock.
//! 2. Fixed clock, named in lowercase (`em01grpaclk`, `pclk`).
//!    - Series 2: [`SERIES2_FIXED`], the peripheral-to-branch table of
//!      `CMU_ClockFreqGet` in the SDK's `em_cmu.c`.
//!    - Series 0: the bus of the gate register (`HFPERCLKEN0` is
//!      `hfperclk`), see [`SERIES0_GATE_CLOCKS`].
//! 3. `None`: the peripheral has no kernel clock of its own (oscillators,
//!    CMU, EMU, ...).
//!
//! Group clocks such as EM01GRPACLK have their own mux, but they are
//! reported as fixed clocks. The HAL computes their frequency when it sets
//! up the clock tree.
//!
//! Series 0 LE prescalers (`LFAPRESC0.LETIMER0`) are not part of the
//! kernel clock. The driver owns them, like the TIMER `PRESC` field.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chiptool::ir::{BitOffset, BlockItemInner, IR};
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

use crate::pac::IpKey;

/// CMU data of one peripheral. At least one field is `Some`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeripheralCmu {
    pub enable: Option<ClockEnable>,
    pub kernel_clock: Option<KernelClock>,
}

/// Clock that drives a peripheral.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelClock {
    /// A fixed clock-tree node, lowercase (`em01grpaclk`).
    Clock(String),
    /// A CMU select field (`eusart0clkctrl`.`clksel`) with an enum.
    Mux { register: String, field: String },
}

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
/// Series 0 bus-specific enable registers and `PCNTCTRL`.
const GATE_REGISTERS: &str = r"^(clken\d+|hfcoreclken0|hfperclken0|lfaclken0|lfbclken0|pcntctrl)$";

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

/// Series 2 fixed kernel clocks as `(peripheral regex, clock)`. Source:
/// `CMU_ClockFreqGet` in `platform/emlib/src/em_cmu.c` of the Simplicity
/// SDK, the `_SILICON_LABS_32B_SERIES_2_CONFIG > 1` variant.
/// Config 1 (xG21) has a different table and is rejected.
const SERIES2_FIXED: &[(&str, &str)] = &[
    (r"^(MSC|LDMA|SMU|RADIOAES|CRYPTOACC|MVP|ICACHE0)$", "hclk"),
    (
        r"^(USART\d|I2C[1-9]|PRS|GPIO|GPCRC|LDMAXBAR|SYSCFG|DCDC|BURAM|DPLL0)$",
        "pclk",
    ),
    (r"^(I2C0|AMUXCP0|ACMP\d)$", "lspclk"),
    (r"^(TIMER\d|KEYSCAN)$", "em01grpaclk"),
    (r"^PDM$", "em01grpbclk"),
    (r"^EUSART[1-9]$", "em01grpcclk"),
    (r"^(LETIMER0|LESENSE)$", "em23grpaclk"),
    (r"^(BURTC|ETAMPDET)$", "em4grpaclk"),
];

/// Series 0 kernel clock per gate register.
const SERIES0_GATE_CLOCKS: &[(&str, &str)] = &[
    ("hfperclken0", "hfperclk"),
    ("hfcoreclken0", "hfcoreclk"),
    ("lfaclken0", "lfaclk"),
    ("lfbclken0", "lfbclk"),
];

/// CMU data per canonical peripheral name for one chip. Peripherals with
/// neither a gate nor a kernel clock are left out.
pub fn peripheral_cmu(
    chip: &ChipFile,
    names: &[String],
    irs: &BTreeMap<IpKey, IR>,
) -> Result<BTreeMap<String, PeripheralCmu>> {
    let series = chip
        .chip
        .series
        .with_context(|| format!("{} has no series", chip.chip.name))?;
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
            // `PCNTCTRL` also holds the 1-bit `PCNTnCLKSEL`, which has an enum.
            if f.bit_size != 1 || f.enumm.is_some() {
                continue;
            }
            let name = f.name.strip_suffix("clken").unwrap_or(&f.name);
            let gate = ClockEnable {
                register: item.name.clone(),
                field: f.name.clone(),
                address: cmu.base_address + u64::from(item.byte_offset),
                bit,
            };
            if let Some(prev) = gates.insert(name.to_owned(), gate) {
                bail!("CMU gate `{name}` is in both {} and {}", prev.register, item.name);
            }
        }
    }

    // Select fields by name: `<name>clkctrl`.`clksel` (Series 2) or
    // `<name>clksel` in any register (Series 0).
    let mut selects: BTreeMap<String, (String, String)> = BTreeMap::new();
    for item in &block.items {
        let BlockItemInner::Register(reg) = &item.inner else {
            continue;
        };
        if item.array.is_some()
            || item.name.ends_with("_set")
            || item.name.ends_with("_clr")
            || item.name.ends_with("_tgl")
        {
            continue;
        }
        let Some(fs) = reg.fieldset.as_ref().and_then(|f| ir.fieldsets.get(f)) else {
            continue;
        };
        for f in fs.fields.iter().filter(|f| f.enumm.is_some()) {
            let key = if f.name == "clksel" {
                item.name.strip_suffix("clkctrl")
            } else {
                f.name.strip_suffix("clksel")
            };
            if let Some(key) = key.filter(|k| !k.is_empty()) {
                selects.insert(key.to_owned(), (item.name.clone(), f.name.clone()));
            }
        }
    }

    let fixed = match series.series {
        2 if series.config == 1 => bail!("{}: Series 2 config 1 kernel clocks are not curated", chip.chip.name),
        2 => SERIES2_FIXED
            .iter()
            .map(|(re, clock)| Ok((Regex::new(re)?, *clock)))
            .collect::<Result<Vec<_>>>()?,
        _ => Vec::new(),
    };

    let mut out = BTreeMap::new();
    for canonical in names {
        let field = OVERRIDES
            .iter()
            .find(|(p, _)| p == canonical)
            .map(|(_, f)| (*f).to_owned())
            .unwrap_or_else(|| canonical.to_ascii_lowercase());
        let enable = gates.get(&field).cloned();

        let stripped = field.trim_end_matches(|c: char| c.is_ascii_digit());
        let mux = [field.clone(), stripped.to_owned(), format!("{field}0")]
            .into_iter()
            .find_map(|k| selects.get(&k).cloned());
        let kernel_clock = if let Some((register, field)) = mux {
            Some(KernelClock::Mux { register, field })
        } else if let Some((_, clock)) = fixed.iter().find(|(re, _)| re.is_match(canonical)) {
            Some(KernelClock::Clock((*clock).to_owned()))
        } else if series.series == 0 {
            enable.as_ref().and_then(|e| {
                SERIES0_GATE_CLOCKS
                    .iter()
                    .find(|(reg, _)| *reg == e.register)
                    .map(|(_, clock)| KernelClock::Clock((*clock).to_owned()))
            })
        } else {
            None
        };

        if enable.is_some() || kernel_clock.is_some() {
            out.insert(canonical.clone(), PeripheralCmu { enable, kernel_clock });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ir(yaml: &str) -> IR {
        serde_yaml::from_str(yaml).unwrap()
    }

    const S2_CMU: &str = "
block/Cmu:
  items:
  - name: clken0
    byte_offset: 100
    fieldset: regs::Clken0
  - name: clken1
    byte_offset: 104
    fieldset: regs::Clken1
  - name: iadcclkctrl
    byte_offset: 288
    fieldset: regs::Iadcclkctrl
  - name: eusart0clkctrl
    byte_offset: 292
    fieldset: regs::Eusart0clkctrl
  - name: clken0_set
    byte_offset: 4196
    access: Write
    fieldset: regs::Clken0
fieldset/regs::Clken0:
  fields:
  - name: timer0
    bit_offset: 5
    bit_size: 1
  - name: iadc0
    bit_offset: 10
    bit_size: 1
fieldset/regs::Clken1:
  fields:
  - name: timer4
    bit_offset: 31
    bit_size: 1
  - name: dmem
    bit_offset: 2
    bit_size: 1
  - name: eusart0
    bit_offset: 3
    bit_size: 1
  - name: eusart1
    bit_offset: 4
    bit_size: 1
fieldset/regs::Iadcclkctrl:
  fields:
  - name: clksel
    bit_offset: 0
    bit_size: 2
    enum: vals::IadcclkctrlClksel
fieldset/regs::Eusart0clkctrl:
  fields:
  - name: clksel
    bit_offset: 0
    bit_size: 3
    enum: vals::Eusart0clkctrlClksel
enum/vals::IadcclkctrlClksel:
  bit_size: 2
  variants:
  - name: Em01grpaclk
    value: 1
enum/vals::Eusart0clkctrlClksel:
  bit_size: 3
  variants:
  - name: Lfxo
    value: 3
";

    fn chip(series: u8, config: u16, cmu_version: &str) -> ChipFile {
        serde_json::from_value(serde_json::json!({
            "chip": { "name": "FAKE", "core": "CM33", "fpu": true, "mpu": true, "trustzone": true,
                      "memory": [], "svd": "x.svd", "series": { "series": series, "config": config } },
            "peripherals": [
                { "name": "CMU_NS", "base_address": 0x5000_8000u64, "version": "3",
                  "kind": "cmu", "register_version": cmu_version, "block": "Cmu" }
            ],
            "interrupts": []
        }))
        .unwrap()
    }

    fn run(
        series: u8,
        config: u16,
        cmu_version: &str,
        yaml: &str,
        names: &[&str],
    ) -> Result<BTreeMap<String, PeripheralCmu>> {
        let mut irs = BTreeMap::new();
        irs.insert(("cmu".to_owned(), cmu_version.to_owned()), ir(yaml));
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        peripheral_cmu(&chip(series, config, cmu_version), &names, &irs)
    }

    fn clock(name: &str) -> Option<KernelClock> {
        Some(KernelClock::Clock(name.to_owned()))
    }

    fn mux(register: &str) -> Option<KernelClock> {
        Some(KernelClock::Mux {
            register: register.to_owned(),
            field: "clksel".to_owned(),
        })
    }

    #[test]
    fn gates_match_by_name_and_override_and_skip_aliases() {
        let got = run(2, 4, "s2v3", S2_CMU, &["TIMER0", "TIMER4", "DMEM1", "EMU"]).unwrap();
        assert_eq!(
            got["TIMER4"].enable,
            Some(ClockEnable {
                register: "clken1".into(),
                field: "timer4".into(),
                address: 0x5000_8068,
                bit: 31
            })
        );
        let timer0 = got["TIMER0"].enable.as_ref().unwrap();
        assert_eq!(timer0.register, "clken0", "must not pick the clken0_set alias");
        assert_eq!(got["DMEM1"].enable.as_ref().unwrap().field, "dmem");
        assert!(!got.contains_key("EMU"));
    }

    #[test]
    fn series2_kernel_clocks_prefer_the_own_mux() {
        let got = run(
            2,
            4,
            "s2v3",
            S2_CMU,
            &["TIMER4", "EUSART0", "EUSART1", "IADC0", "DMEM1"],
        )
        .unwrap();
        assert_eq!(got["TIMER4"].kernel_clock, clock("em01grpaclk"));
        assert_eq!(got["EUSART0"].kernel_clock, mux("eusart0clkctrl"));
        assert_eq!(got["EUSART1"].kernel_clock, clock("em01grpcclk"));
        assert_eq!(got["IADC0"].kernel_clock, mux("iadcclkctrl"), "IADC0 uses IADCCLKCTRL");
        assert_eq!(got["DMEM1"].kernel_clock, None);
    }

    #[test]
    fn series2_config1_is_rejected() {
        assert!(run(2, 1, "s2v3", S2_CMU, &["TIMER0"]).is_err());
    }

    #[test]
    fn series0_kernel_clock_is_the_gate_bus() {
        let yaml = "
block/Cmu:
  items:
  - name: hfperclken0
    byte_offset: 68
    fieldset: regs::Hfperclken0
  - name: lfaclken0
    byte_offset: 88
    fieldset: regs::Lfaclken0
  - name: pcntctrl
    byte_offset: 120
    fieldset: regs::Pcntctrl
fieldset/regs::Hfperclken0:
  fields:
  - name: timer0
    bit_offset: 5
    bit_size: 1
fieldset/regs::Lfaclken0:
  fields:
  - name: letimer0
    bit_offset: 2
    bit_size: 1
fieldset/regs::Pcntctrl:
  fields:
  - name: pcnt0clken
    bit_offset: 0
    bit_size: 1
  - name: pcnt0clksel
    bit_offset: 1
    bit_size: 1
    enum: vals::Pcntclksel
enum/vals::Pcntclksel:
  bit_size: 1
  variants:
  - name: Lfaclk
    value: 0
";
        let got = run(0, 0, "s0v1", yaml, &["TIMER0", "LETIMER0", "PCNT0"]).unwrap();
        assert_eq!(got["TIMER0"].kernel_clock, clock("hfperclk"));
        assert_eq!(got["LETIMER0"].kernel_clock, clock("lfaclk"));
        assert_eq!(
            got["PCNT0"].kernel_clock,
            Some(KernelClock::Mux {
                register: "pcntctrl".into(),
                field: "pcnt0clksel".into()
            })
        );
        let pcnt0 = got["PCNT0"].enable.as_ref().unwrap();
        assert_eq!(
            (pcnt0.register.as_str(), pcnt0.field.as_str(), pcnt0.bit),
            ("pcntctrl", "pcnt0clken", 0)
        );
    }
}
