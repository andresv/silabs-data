//! Regex-keyed routing from `(chip, peripheral, svd_version)` to
//! `(kind, version, block)`.
//!
//! `perimap` decides which curated register YAML a peripheral instance uses.
//! When no entry matches, `default_route` derives the route from the SVD
//! peripheral name and `<version>` tag. Entries override that default for:
//!
//! - structural splits that the SVD merges (TIMER0/1 are 32-bit, TIMER2+ are
//!   16-bit, but all claim `<version>1</version>`);
//! - cosmetic renames (drop `_NS` from the block name);
//! - fixed version labels, so vendor SVD drift cannot change the routing.
//!
//! First match wins. Order entries from most-specific to least-specific.

use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

/// One perimap entry: a key regex over `<chip>:<peripheral>:<svd_version>`
/// and a target `(kind, version, block)` triple.
pub struct Entry {
    pub key: Regex,
    pub kind: &'static str,
    pub version: &'static str,
    pub block: &'static str,
}

/// Result of routing a peripheral instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub kind: String,
    pub version: String,
    pub block: String,
}

/// Static perimap entries. Add specific overrides above generic catch-alls.
///
/// Naming convention:
/// - `kind` is lowercase, no `_NS` / `_S` suffix, no trailing digits.
///   `gpio`, `eusart`, `timer`, `cmu`, etc.
/// - `version` is `s<series>v<N>`: the chip's series, then the SVD
///   `<version>` tag where it is reliable (`s2v1`). A descriptive suffix
///   (`s2v0_mg24`) can follow, where the SVD merges incompatible variants.
///   Every label starts with its series, so version cfgs such as
///   `timer_s2` and `timer_s0` look the same on every series.
/// - `block` is the canonical block name in the curated YAML, no suffix.
pub static ENTRIES: &[(&str, &str, &str, &str)] = &[
    // (key_regex, kind, version, block)
    //
    // EUSART needs no row. All instances report <version>2</version>.
    // EUSART0 adds a few low-frequency registers and fields that EUSART1+
    // reserve. So `eusart_s2v2.yaml` is their superset, and every instance
    // has the same type.
    // TIMER bit-width split. Wide (32-bit) and narrow (16-bit) timers
    // share <version>1</version> in the SVD but differ in the bit_size of
    // CNT / TOP / CCx. Both are blocks of one `timer_s2v1.yaml`: `Timer`
    // (16-bit) and `Timer32` (32-bit). The instance grouping comes from a
    // diff of the extracted IRs of every instance on both packs:
    //   MG24: TIMER0/1 wide, TIMER2..4 narrow.
    //   MG26: TIMER0/1/8/9 wide, TIMER2..7 narrow.
    ("EFR32MG2[46].*:TIMER[01]_NS:.*", "timer", "s2v1", "TIMER32"),
    ("EFR32MG26.*:TIMER[89]_NS:.*", "timer", "s2v1", "TIMER32"),
    ("EFR32MG2[46].*:TIMER[2-7]_NS:.*", "timer", "s2v1", "TIMER"),
    // IADC needs no row. The high-accuracy sub-families add an OSRHA field,
    // HIGHACCURACY / HIGHSPEED ADCMODE and VREF2P5. FG25 adds a LESENSE scan
    // trigger. These only add fields and enum values, so `iadc_s2v3.yaml` is
    // the superset of every <version>3</version> IADC.
    // SMU, SMU_*_CFGNS, SYSCFG, DMEM and VDAC need no rows. Chips with MVP
    // add MVP access-control and port-select fields. MG24's DMEM adds
    // CTRL.WAITSTATES. FG25's DMEM has two AHB ports instead of four, and its
    // VDAC adds a LESENSE trigger. None of that moves or resizes a field, so
    // each (kind, version) is one superset YAML.
    // DEVINFO is a per-family factory-programmed block. Both families
    // report <version>0.0</version>, but their register layouts differ
    // (calibration data, chip-specific fields).
    ("EFR32MG24.*:DEVINFO:.*", "devinfo", "s2v0_mg24", "DEVINFO"),
    ("EFR32MG26.*:DEVINFO:.*", "devinfo", "s2v0_mg26", "DEVINFO"),
    // --- EFR32FG25 (Series 2, config 5) ---
    // TIMER bit-width split, same rationale as the MG2x entries above.
    // FG25 ships TIMER0..7; TIMER0/1 are 32-bit wide, TIMER2..7 are 16-bit
    // narrow. Reuse the shared timer_s2v1 blocks.
    ("EFR32FG25.*:TIMER[01]_NS:.*", "timer", "s2v1", "TIMER32"),
    ("EFR32FG25.*:TIMER[2-7]_NS:.*", "timer", "s2v1", "TIMER"),
    // --- EFR32MG22 (Series 2, config 2) ---
    // TIMER split: on MG22 only TIMER0 is the wide (32-bit) timer. TIMER1..4
    // are 16-bit. They share <version>0</version> and differ only in field
    // widths, so both are blocks of `timer_s2v0.yaml` (`Timer32` for TIMER0,
    // `Timer` for the rest).
    ("EFR32MG22.*:TIMER0_NS:.*", "timer", "s2v0", "TIMER32"),
    ("EFR32MG22.*:TIMER[1-4]_NS:.*", "timer", "s2v0", "TIMER"),
    // --- EFM32GG (Series 0 Giant Gecko) ---
    // UART0/1 are the asynchronous subset of the USART IP and share its
    // register layout exactly (verified by extraction: only one enum-variant
    // description differs). Route them to the shared usart block. Series 0
    // SVDs carry no <version>, so the key's version part is empty.
    ("EFM32GG[0-9]{3}F.*:UART[0-9]+:", "usart", "s0v1", "USART"),
];

/// Version labels for chips whose SVDs carry no `<peripheral><version>`
/// tag (every EFM32/EFR32 Series 0 and Series 1 pack). Key: chip-name
/// regex (anchored). Value: the label every unversioned peripheral on a
/// matching chip gets, unless an `ENTRIES` row matches first.
///
/// Label scheme: `s<series>v<N>`, numbered per series in release order.
/// Versioned SVDs (Series 2+) get `s<series>v<svd_version>` instead, so
/// the two schemes never mix within a series. When a new family shares
/// most blocks with an onboarded one, give it the same label here. Then
/// split the blocks that differ with `ENTRIES` rows (e.g. `gpio` → `s0v2`).
pub static UNVERSIONED: &[(&str, &str)] = &[
    // EFM32GG (Series 0). `[0-9]{3}F` excludes Series 1 EFM32GG11B/GG12B.
    ("EFM32GG[0-9]{3}F.*", "s0v1"),
];

fn unversioned_label(chip: &str) -> Option<&'static str> {
    static COMPILED: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let compiled = COMPILED.get_or_init(|| {
        UNVERSIONED
            .iter()
            .map(|(key, label)| {
                (
                    Regex::new(&format!("^{key}$")).expect("UNVERSIONED regex compiles"),
                    *label,
                )
            })
            .collect()
    });
    compiled
        .iter()
        .find(|(re, _)| re.is_match(chip))
        .map(|(_, label)| *label)
}

/// Compile the static `ENTRIES` table into runtime `Entry`s.
pub fn compile() -> Result<Vec<Entry>> {
    ENTRIES
        .iter()
        .map(|(key, kind, version, block)| {
            let re = Regex::new(&format!("^{key}$")).with_context(|| format!("perimap key `{key}`"))?;
            Ok(Entry {
                key: re,
                kind,
                version,
                block,
            })
        })
        .collect()
}

/// Default routing: derive `(kind, version, block)` from the raw SVD inputs
/// when no perimap entry matches.
///
/// - kind / block: peripheral name with `_NS` or `_S` stripped, then any
///   trailing ASCII digits.
/// - version: `s<series>v<svd_version>` (where `svd_version` is the SVD
///   tag, or `unknown` if the SVD doesn't carry one).
fn default_route(peripheral: &str, svd_version: Option<&str>, series: u8) -> Route {
    let stripped = peripheral
        .strip_suffix("_NS")
        .or_else(|| peripheral.strip_suffix("_S"))
        .unwrap_or(peripheral);
    let trimmed = stripped.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = if trimmed.is_empty() { stripped } else { trimmed };
    let kind = base.to_ascii_lowercase();
    let version = match svd_version {
        Some(v) if !v.is_empty() => format!("s{series}v{}", sanitise_version(v)),
        _ => format!("s{series}vunknown"),
    };
    Route {
        kind,
        version,
        block: base.to_owned(),
    }
}

/// Route a peripheral instance to its `(kind, version, block)`. The
/// `compiled` argument should come from [`compile`]. `series` is the
/// chip's Silicon Labs series (from its CMSIS header).
///
/// Fails when the SVD has no `<version>` for the peripheral and neither
/// `ENTRIES` nor `UNVERSIONED` covers the chip, so no peripheral is ever
/// published under a guessed version.
pub fn route(compiled: &[Entry], chip: &str, peripheral: &str, svd_version: Option<&str>, series: u8) -> Result<Route> {
    let key = format!("{chip}:{peripheral}:{}", svd_version.unwrap_or(""));
    for e in compiled {
        if e.key.is_match(&key) {
            return Ok(Route {
                kind: e.kind.to_owned(),
                version: e.version.to_owned(),
                block: e.block.to_owned(),
            });
        }
    }
    let mut r = default_route(peripheral, svd_version, series);
    if svd_version.is_none_or(str::is_empty) {
        let Some(label) = unversioned_label(chip) else {
            bail!(
                "{chip}:{peripheral} has no SVD <version> and no perimap route — \
                 add the chip family to perimap::UNVERSIONED or add an ENTRIES row"
            );
        };
        r.version = label.to_owned();
    }
    Ok(r)
}

/// Sanitise an SVD version string to a Rust-identifier-friendly suffix.
/// Keep ASCII alphanumerics; replace everything else with `_`.
pub fn sanitise_version(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_strips_ns_and_digits() {
        let r = default_route("EUSART0_NS", Some("2"), 2);
        assert_eq!(r.kind, "eusart");
        assert_eq!(r.version, "s2v2");
        assert_eq!(r.block, "EUSART");
    }

    #[test]
    fn default_handles_no_suffix() {
        let r = default_route("CMU", Some("3"), 2);
        assert_eq!(r.kind, "cmu");
        assert_eq!(r.version, "s2v3");
        assert_eq!(r.block, "CMU");
    }

    #[test]
    fn default_falls_back_when_no_svd_version() {
        let r = default_route("GPIO_NS", None, 2);
        assert_eq!(r.version, "s2vunknown");
    }

    #[test]
    fn empty_perimap_uses_default_for_known_silabs_names() {
        let compiled = compile().unwrap();
        let r = route(&compiled, "EFR32MG26B211F2048IM68", "GPIO_NS", Some("7"), 2).unwrap();
        assert_eq!(r.kind, "gpio");
        assert_eq!(r.version, "s2v7");
        assert_eq!(r.block, "GPIO");
    }

    #[test]
    fn unversioned_efm32gg_gets_family_label() {
        let compiled = compile().unwrap();
        let r = route(&compiled, "EFM32GG390F1024", "TIMER0", None, 0).unwrap();
        assert_eq!(
            (r.kind.as_str(), r.version.as_str(), r.block.as_str()),
            ("timer", "s0v1", "TIMER")
        );
    }

    #[test]
    fn efm32gg_uart_routes_to_usart() {
        let compiled = compile().unwrap();
        let r = route(&compiled, "EFM32GG390F1024", "UART1", None, 0).unwrap();
        assert_eq!(
            (r.kind.as_str(), r.version.as_str(), r.block.as_str()),
            ("usart", "s0v1", "USART")
        );
    }

    #[test]
    fn unversioned_without_family_label_is_an_error() {
        let compiled = compile().unwrap();
        // EFM32GG11B is Series 1 and not onboarded yet. `[0-9]{3}F` in the
        // GG pattern must not match it.
        let err = route(&compiled, "EFM32GG11B820F2048GL192", "GPIO", None, 1).unwrap_err();
        assert!(err.to_string().contains("UNVERSIONED"), "{err}");
    }

    #[test]
    fn unversioned_patterns_compile() {
        for (key, _) in UNVERSIONED {
            Regex::new(&format!("^{key}$")).unwrap_or_else(|e| panic!("UNVERSIONED `{key}`: {e}"));
        }
    }
}
