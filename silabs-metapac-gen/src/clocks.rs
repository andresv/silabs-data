//! Per-peripheral CMU data: the clock-gate bit (`enable`) and the clock
//! that drives the peripheral (`kernel_clock`).
//!
//! - **Gate:** the CMU gate field with the instance name (Series 2
//!   `CLKENn.TIMER4`, Series 0 `HFPERCLKEN0.TIMER0`, `PCNTCTRL.PCNT0CLKEN`
//!   without `CLKEN`). [`OVERRIDES`] covers the names that do not match. No
//!   gate (CMU, EMU, DEVINFO, ...) gives `None`. The address and bit are
//!   resolved here, so a consumer needs no IR walk.
//! - **Kernel clock**, first match:
//!   1. Mux: the peripheral's own CMU select, Series 2
//!      `<name>CLKCTRL.CLKSEL`, Series 0 `<name>CLKSEL`
//!      (`PCNTCTRL.PCNT0CLKSEL`). Each enum variant names a source.
//!   2. Fixed clock, in lowercase: Series 2 [`SERIES2_FIXED`] (from
//!      `CMU_ClockFreqGet` in the SDK's `em_cmu.c`), Series 0 the bus of the
//!      gate register ([`SERIES0_GATE_CLOCKS`]). Group clocks such as
//!      EM01GRPACLK are fixed here. The HAL computes their frequency.
//!   3. `None` (oscillators, CMU, EMU, ...).
//! - **Prescaler** (Series 0 LFACLK/LFBCLK): the field with the instance name
//!   in `LFAPRESC0`/`LFBPRESC0`. Its enum values are `Div<N>`, but the raw
//!   encoding differs (`RTC`, `LETIMER0`: log2(N); `LCD`: log2(N) - 4), so a
//!   HAL takes N from the enum.
//! - **Bus enable** (Series 0): a shared second gate, [`SERIES0_BUS_ENABLES`].
//! - **LF sync** (Series 0): a write to an "(Async Reg)" register
//!   (`LFACLKEN0`, `LFBCLKEN0`, `LFAPRESC0`, `LFBPRESC0`) waits for
//!   `CMU.SYNCBUSY.<register>`. Generation stops when that field is missing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use chiptool::ir::{BitOffset, BlockItemInner, IR};
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

use crate::crate_layout::block_struct_ident;
use crate::pac::IpKey;

/// CMU data of one peripheral. At least one field is `Some`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeripheralCmu {
    pub enable: Option<ClockEnable>,
    /// A shared gate that must be on before `enable` (`hfcoreclken0.le`).
    pub bus_enable: Option<ClockEnable>,
    pub kernel_clock: Option<KernelClock>,
    /// A prescaler field that divides the kernel clock (`lfapresc0.rtc`).
    pub prescaler: Option<CmuRegister>,
}

/// Clock that drives a peripheral.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelClock {
    /// A fixed clock-tree node, lowercase (`em01grpaclk`).
    Clock(String),
    /// A CMU select field (`eusart0clkctrl`.`clksel`) with an enum.
    Mux(CmuRegister),
}

/// A field in a CMU register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmuRegister {
    pub register: String,
    pub field: String,
    /// `CMU.SYNCBUSY` field that a write to `register` waits for.
    pub sync_busy: Option<String>,
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
    /// `CMU.SYNCBUSY` field that a write to `register` waits for.
    pub sync_busy: Option<String>,
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

/// Series 0 shared gates that a peripheral needs before its own gate, as
/// `(peripheral regex, gate field)`. The field must be a gate in the CMU IR.
/// Source: the EFM32GG reference manual.
/// - `HFCORECLKEN0.LE` clocks the bus interface to the Low Energy
///   Peripherals (section 11.5, `CMU_HFCORECLKEN0`). Section 5.3.1 lists them:
///   LCD, LETIMER, LEUART, PCNT, RTC, WDOG, LESENSE and BURTC.
///
/// `HFCORECLKEN0.USBC` is not here. Section 15.3.2 enables it after `USB` and
/// after `CMD.USBCCLKSEL`, so it is not a gate to enable first.
pub const SERIES0_BUS_ENABLES: &[(&str, &str)] = &[(r"^(LCD|LETIMER\d|LEUART\d|PCNT\d|RTC|WDOG|LESENSE|BURTC)$", "le")];

/// CMU registers that hold the Series 0 per-peripheral LF prescalers.
const PRESCALER_REGISTERS: &str = r"^lf[a-z]presc\d+$";

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
    let block_name = block_struct_ident(&cmu.block);
    let block = ir
        .blocks
        .get(&block_name)
        .with_context(|| format!("CMU IR has no block `{block_name}`"))?;
    let sync_busy = sync_busy_fields(ir, block)?;
    let sync = |register: &str| sync_busy.contains(register).then(|| register.to_owned());

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
                sync_busy: sync(&item.name),
            };
            if let Some(prev) = gates.insert(name.to_owned(), gate) {
                bail!("CMU gate `{name}` is in both {} and {}", prev.register, item.name);
            }
        }
    }

    // Select fields by name: `<name>clkctrl`.`clksel` (Series 2) or
    // `<name>clksel` in any register (Series 0).
    let mut selects: BTreeMap<String, CmuRegister> = BTreeMap::new();
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
                let select = CmuRegister {
                    register: item.name.clone(),
                    field: f.name.clone(),
                    sync_busy: sync(&item.name),
                };
                selects.insert(key.to_owned(), select);
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

    // Prescaler fields by name: `lfapresc0.rtc`. Each must have an enum of
    // `Div<N>` variants.
    let presc_re = Regex::new(PRESCALER_REGISTERS).expect("PRESCALER_REGISTERS compiles");
    let mut prescalers: BTreeMap<String, CmuRegister> = BTreeMap::new();
    for item in &block.items {
        let BlockItemInner::Register(reg) = &item.inner else {
            continue;
        };
        if !presc_re.is_match(&item.name) || item.array.is_some() {
            continue;
        }
        let Some(fs) = reg.fieldset.as_ref().and_then(|f| ir.fieldsets.get(f)) else {
            continue;
        };
        for f in &fs.fields {
            check_prescaler_enum(ir, &item.name, &f.name, f.enumm.as_deref())?;
            let presc = CmuRegister {
                register: item.name.clone(),
                field: f.name.clone(),
                sync_busy: sync(&item.name),
            };
            if let Some(prev) = prescalers.insert(f.name.clone(), presc) {
                bail!(
                    "CMU prescaler `{}` is in both {} and {}",
                    f.name,
                    prev.register,
                    item.name
                );
            }
        }
    }

    let bus_enables = match series.series {
        0 => SERIES0_BUS_ENABLES
            .iter()
            .map(|(re, field)| {
                let gate = gates
                    .get(*field)
                    .with_context(|| format!("SERIES0_BUS_ENABLES: the CMU has no gate `{field}`"))?;
                Ok((Regex::new(re)?, gate.clone()))
            })
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
        let kernel_clock = if let Some(mux) = mux {
            Some(KernelClock::Mux(mux))
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

        let bus_enable = bus_enables
            .iter()
            .find(|(re, _)| re.is_match(canonical))
            .map(|(_, gate)| gate.clone());
        // An LF gate needs the LE interface: check that the table agrees.
        if let Some(e) = &enable
            && SERIES0_LF_GATES.contains(&e.register.as_str())
            && bus_enable.is_none()
        {
            bail!(
                "{canonical}: its gate is {}.{}, but SERIES0_BUS_ENABLES has no entry for it",
                e.register,
                e.field
            );
        }
        let prescaler = if enable
            .as_ref()
            .is_some_and(|e| SERIES0_LF_GATES.contains(&e.register.as_str()))
        {
            prescalers.get(&field).cloned()
        } else {
            None
        };

        if enable.is_some() || kernel_clock.is_some() || bus_enable.is_some() {
            out.insert(
                canonical.clone(),
                PeripheralCmu {
                    enable,
                    bus_enable,
                    kernel_clock,
                    prescaler,
                },
            );
        }
    }
    Ok(out)
}

/// Series 0 gate registers in the LF clock domain.
const SERIES0_LF_GATES: &[&str] = &["lfaclken0", "lfbclken0"];

/// The fields of `CMU.SYNCBUSY`. Each CMU register that the IR marks
/// "(Async Reg)" must have one, with the register's name.
fn sync_busy_fields(ir: &IR, block: &chiptool::ir::Block) -> Result<BTreeSet<String>> {
    let mut fields = BTreeSet::new();
    if let Some(item) = block.items.iter().find(|i| i.name == "syncbusy")
        && let BlockItemInner::Register(reg) = &item.inner
        && let Some(fs) = reg.fieldset.as_ref().and_then(|f| ir.fieldsets.get(f))
    {
        fields.extend(fs.fields.iter().map(|f| f.name.clone()));
    }
    for item in &block.items {
        let is_async = item.description.as_deref().is_some_and(|d| d.contains("(Async Reg)"));
        if is_async && !fields.contains(&item.name) {
            bail!(
                "CMU register `{}` is an Async Reg, but CMU.SYNCBUSY has no `{}` field",
                item.name,
                item.name
            );
        }
    }
    Ok(fields)
}

/// A prescaler field must have an enum whose variants are all `Div<N>`, with
/// N a power of two.
fn check_prescaler_enum(ir: &IR, register: &str, field: &str, enumm: Option<&str>) -> Result<()> {
    let e = enumm
        .and_then(|e| ir.enums.get(e))
        .with_context(|| format!("CMU prescaler {register}.{field} has no enum"))?;
    for v in &e.variants {
        let n: u32 = v
            .name
            .strip_prefix("Div")
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("CMU prescaler {register}.{field}: variant `{}` is not `Div<N>`", v.name))?;
        ensure!(
            n.is_power_of_two(),
            "CMU prescaler {register}.{field}: {} is not a power of two",
            v.name
        );
    }
    Ok(())
}

/// Clock names in the kernel-clock, mux and prescaler data of a chip,
/// sorted: each `Clock` name, and each source of each `Mux` field. A source
/// name is the lowercase enum variant name. `Disabled` is not a clock. A
/// `rt` suffix (`Hfxort`, the retimed copy) and a `div<N>` suffix
/// (`Hclkdiv1024`) are dropped, so the name is the source clock. A
/// prescaler divides the kernel clock, so it adds no name.
pub fn clock_names(
    chip: &ChipFile,
    cmu: &BTreeMap<String, PeripheralCmu>,
    irs: &BTreeMap<IpKey, IR>,
) -> Result<Vec<String>> {
    let cmu_p = chip
        .peripherals
        .iter()
        .find(|p| p.kind == "cmu" && !p.name.ends_with("_S"))
        .with_context(|| format!("{} has no CMU", chip.chip.name))?;
    let ir = irs
        .get(&(cmu_p.kind.clone(), cmu_p.register_version.clone()))
        .with_context(|| format!("no IR for cmu_{}", cmu_p.register_version))?;
    let block_name = block_struct_ident(&cmu_p.block);
    let block = ir
        .blocks
        .get(&block_name)
        .with_context(|| format!("CMU IR has no block `{block_name}`"))?;
    let mut names = BTreeSet::new();
    for c in cmu.values() {
        match &c.kernel_clock {
            None => {}
            Some(KernelClock::Clock(name)) => {
                names.insert(name.clone());
            }
            Some(KernelClock::Mux(mux)) => {
                let item = block
                    .items
                    .iter()
                    .find(|i| i.name == mux.register)
                    .with_context(|| format!("CMU has no register {}", mux.register))?;
                let BlockItemInner::Register(reg) = &item.inner else {
                    bail!("CMU {} is not a register", mux.register);
                };
                let e = reg
                    .fieldset
                    .as_ref()
                    .and_then(|f| ir.fieldsets.get(f))
                    .and_then(|fs| fs.fields.iter().find(|f| f.name == mux.field))
                    .and_then(|f| f.enumm.as_ref())
                    .and_then(|e| ir.enums.get(e))
                    .with_context(|| format!("CMU {}.{} has no enum", mux.register, mux.field))?;
                let variants: Vec<String> = e.variants.iter().map(|v| v.name.to_ascii_lowercase()).collect();
                names.extend(variants.iter().filter_map(|v| mux_source_name(v, &variants)));
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// The source clock of a lowercase mux enum variant. `None` for
/// `disabled`. A `rt` suffix is dropped only when the enum also has the
/// base name (`hfxort` and `hfxo`).
fn mux_source_name(variant: &str, variants: &[String]) -> Option<String> {
    if variant.starts_with("disable") {
        return None;
    }
    let v = match variant.split_once("div") {
        Some((base, n)) if !base.is_empty() && n.parse::<u32>().is_ok() => base,
        _ => variant,
    };
    let v = match v.strip_suffix("rt") {
        Some(base) if variants.iter().any(|x| x == base) => base,
        _ => v,
    };
    Some(v.to_owned())
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
        Some(KernelClock::Mux(CmuRegister {
            register: register.to_owned(),
            field: "clksel".to_owned(),
            sync_busy: None,
        }))
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
                bit: 31,
                sync_busy: None,
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

    const S0_CMU: &str = "
block/Cmu:
  items:
  - name: hfcoreclken0
    byte_offset: 64
    fieldset: regs::Hfcoreclken0
  - name: hfperclken0
    byte_offset: 68
    fieldset: regs::Hfperclken0
  - name: syncbusy
    byte_offset: 80
    access: Read
    fieldset: regs::Syncbusy
  - name: lfaclken0
    description: Low Frequency A Clock Enable Register 0 (Async Reg).
    byte_offset: 88
    fieldset: regs::Lfaclken0
  - name: lfapresc0
    description: Low Frequency A Prescaler Register 0 (Async Reg).
    byte_offset: 104
    fieldset: regs::Lfapresc0
  - name: pcntctrl
    byte_offset: 120
    fieldset: regs::Pcntctrl
fieldset/regs::Hfcoreclken0:
  fields:
  - name: usbc
    bit_offset: 2
    bit_size: 1
  - name: usb
    bit_offset: 3
    bit_size: 1
  - name: le
    bit_offset: 4
    bit_size: 1
fieldset/regs::Hfperclken0:
  fields:
  - name: timer0
    bit_offset: 5
    bit_size: 1
fieldset/regs::Syncbusy:
  fields:
  - name: lfaclken0
    bit_offset: 0
    bit_size: 1
  - name: lfapresc0
    bit_offset: 2
    bit_size: 1
fieldset/regs::Lfaclken0:
  fields:
  - name: rtc
    bit_offset: 1
    bit_size: 1
  - name: letimer0
    bit_offset: 2
    bit_size: 1
fieldset/regs::Lfapresc0:
  fields:
  - name: rtc
    bit_offset: 4
    bit_size: 4
    enum: vals::Rtc
  - name: letimer0
    bit_offset: 8
    bit_size: 4
    enum: vals::Rtc
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
  - name: Pcnts0
    value: 1
enum/vals::Rtc:
  bit_size: 4
  variants:
  - name: Div1
    value: 0
  - name: Div32768
    value: 15
";

    const S0_NAMES: &[&str] = &["TIMER0", "LETIMER0", "PCNT0", "RTC", "USB", "WDOG", "GPIO"];

    #[test]
    fn series0_kernel_clock_is_the_gate_bus() {
        let got = run(0, 0, "s0v1", S0_CMU, S0_NAMES).unwrap();
        assert_eq!(got["TIMER0"].kernel_clock, clock("hfperclk"));
        assert_eq!(got["LETIMER0"].kernel_clock, clock("lfaclk"));
        assert_eq!(
            got["PCNT0"].kernel_clock,
            Some(KernelClock::Mux(CmuRegister {
                register: "pcntctrl".into(),
                field: "pcnt0clksel".into(),
                sync_busy: None,
            }))
        );
        let pcnt0 = got["PCNT0"].enable.as_ref().unwrap();
        assert_eq!(
            (pcnt0.register.as_str(), pcnt0.field.as_str(), pcnt0.bit),
            ("pcntctrl", "pcnt0clken", 0)
        );
    }

    fn gate(register: &str, field: &str, offset: u64, bit: u32, sync: Option<&str>) -> Option<ClockEnable> {
        Some(ClockEnable {
            register: register.into(),
            field: field.into(),
            address: 0x5000_8000 + offset,
            bit,
            sync_busy: sync.map(Into::into),
        })
    }

    #[test]
    fn series0_bus_enable_sync_busy_and_prescaler() {
        let got = run(0, 0, "s0v1", S0_CMU, S0_NAMES).unwrap();
        let le = gate("hfcoreclken0", "le", 64, 4, None);
        // LE peripherals need HFCORECLKEN0.LE. USB has no bus enable.
        assert_eq!(got["RTC"].bus_enable, le);
        assert_eq!(got["LETIMER0"].bus_enable, le);
        assert_eq!(got["PCNT0"].bus_enable, le);
        assert_eq!(got["USB"].bus_enable, None);
        assert_eq!(got["TIMER0"].bus_enable, None);
        // WDOG has no gate and no kernel clock, but needs the LE interface.
        assert_eq!(
            got["WDOG"],
            PeripheralCmu {
                bus_enable: le.clone(),
                ..Default::default()
            }
        );
        assert!(!got.contains_key("GPIO"));
        // An LF gate waits for SYNCBUSY.LFACLKEN0. An HF gate does not.
        assert_eq!(got["RTC"].enable, gate("lfaclken0", "rtc", 88, 1, Some("lfaclken0")));
        assert_eq!(got["TIMER0"].enable.as_ref().unwrap().sync_busy, None);
        // The LF prescaler named after the peripheral, with its sync field.
        let presc = |field: &str| {
            Some(CmuRegister {
                register: "lfapresc0".into(),
                field: field.into(),
                sync_busy: Some("lfapresc0".into()),
            })
        };
        assert_eq!(got["RTC"].prescaler, presc("rtc"));
        assert_eq!(got["LETIMER0"].prescaler, presc("letimer0"));
        assert_eq!(got["PCNT0"].prescaler, None);
        assert_eq!(got["TIMER0"].prescaler, None);
    }

    #[test]
    fn series2_has_no_bus_enable_sync_or_prescaler() {
        let got = run(2, 4, "s2v3", S2_CMU, &["TIMER0", "TIMER4", "EUSART0"]).unwrap();
        for c in got.values() {
            assert_eq!(c.bus_enable, None);
            assert_eq!(c.prescaler, None);
            assert_eq!(c.enable.as_ref().unwrap().sync_busy, None);
        }
    }

    #[test]
    fn async_register_needs_a_syncbusy_field() {
        let yaml = S0_CMU.replace("  - name: lfapresc0\n    bit_offset: 2\n    bit_size: 1\n", "");
        let err = run(0, 0, "s0v1", &yaml, S0_NAMES).unwrap_err().to_string();
        assert!(err.contains("`lfapresc0` is an Async Reg"), "{err}");
    }

    #[test]
    fn prescaler_enum_must_name_the_divider() {
        let yaml = S0_CMU.replace("  - name: Div32768\n", "  - name: Slow\n");
        let err = run(0, 0, "s0v1", &yaml, S0_NAMES).unwrap_err().to_string();
        assert!(err.contains("`Slow` is not `Div<N>`"), "{err}");
        // An LF gate without a SERIES0_BUS_ENABLES entry stops the generator.
        let yaml = S0_CMU.replace(
            "  - name: letimer0\n    bit_offset: 2\n",
            "  - name: vdac9\n    bit_offset: 2\n",
        );
        let err = run(0, 0, "s0v1", &yaml, &["VDAC9"]).unwrap_err().to_string();
        assert!(err.contains("SERIES0_BUS_ENABLES has no entry"), "{err}");
    }

    #[test]
    fn clock_names_cover_kernel_clocks_and_mux_sources() {
        let names = |series: u8, config: u16, version: &str, yaml: &str, peris: &[&str]| {
            let mut irs = BTreeMap::new();
            irs.insert(("cmu".to_owned(), version.to_owned()), ir(yaml));
            let chip = chip(series, config, version);
            let peris: Vec<String> = peris.iter().map(|n| (*n).to_owned()).collect();
            let cmu = peripheral_cmu(&chip, &peris, &irs).unwrap();
            clock_names(&chip, &cmu, &irs).unwrap()
        };
        assert_eq!(
            names(0, 0, "s0v1", S0_CMU, S0_NAMES),
            ["hfcoreclk", "hfperclk", "lfaclk", "pcnts0"]
        );
        // `Lfxo` from the EUSART0 mux, and the fixed clocks of TIMER4 and EUSART1.
        assert_eq!(
            names(2, 4, "s2v3", S2_CMU, &["TIMER4", "EUSART0", "EUSART1", "IADC0"]),
            ["em01grpaclk", "em01grpcclk", "lfxo"]
        );
        let all = ["hfxo", "hfxort", "hclkdiv1024", "disabled", "pcnts0"].map(String::from);
        let got: Vec<Option<String>> = all.iter().map(|v| mux_source_name(v, &all)).collect();
        assert_eq!(
            got,
            [Some("hfxo"), Some("hfxo"), Some("hclk"), None, Some("pcnts0")].map(|o| o.map(String::from))
        );
    }
}
