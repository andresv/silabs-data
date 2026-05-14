//! Regex-keyed routing from `(chip, peripheral, svd_version)` to
//! `(kind, version, block)`.
//!
//! `perimap` is the explicit authority on which curated register YAML a
//! peripheral instance maps to. The default routing (used when no entry
//! matches) is derived from the SVD: kind = peripheral name with `_NS` /
//! `_S` / trailing-digits stripped, version = the SVD `<peripheral><version>`
//! tag, block = the stripped kind name. perimap overrides that default for:
//!
//! - structural splits the SVD merges accidentally (e.g. EUSART0 has an LF
//!   sub-block, EUSART1+ don't, but all four claim `<version>2</version>`);
//! - cosmetic renames (drop `_NS` from the block name);
//! - explicit pinning of version labels so vendor SVD drift can't silently
//!   change the routing.
//!
//! First match wins. Order entries from most-specific to least-specific.

use anyhow::{Context, Result};
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
/// - `version` is `v<N>` matching the SVD `<version>` tag where reliable,
///   or a descriptive label (`v2_lf`) where SVD merges incompatible
///   variants.
/// - `block` is the canonical block name in the curated YAML, no suffix.
pub static ENTRIES: &[(&str, &str, &str, &str)] = &[
    // (key_regex, kind, version, block)
    //
    // EUSART variant split. EUSART0 carries the LF (low-frequency) sub-block
    // with IRLFCFG / TIMINGCFG / IRHFCFG-LF; EUSART1-3 don't. All four
    // peripherals report <version>2</version> in the SVD, but their register
    // layouts differ structurally. Route EUSART0 to `eusart_v2_lf.yaml` and
    // the others to `eusart_v2.yaml`.
    ("EFR32MG2[46].*:EUSART0_NS:.*", "eusart", "v2_lf", "EUSART"),
    ("EFR32MG2[46].*:EUSART[1-9]_NS:.*", "eusart", "v2", "EUSART"),
    // TIMER bit-width split. Wide (32-bit) and narrow (16-bit) timers
    // share <version>1</version> in the SVD but differ in the bit_size of
    // CNT / TOP / CCx. The instance-number grouping is:
    //   MG24: TIMER0/1 wide, TIMER2..4 narrow.
    //   MG26: TIMER0/1/8/9 wide, TIMER2..7 narrow.
    // Verified by diff'ing extracted IRs across all instances on both packs.
    ("EFR32MG2[46].*:TIMER[01]_NS:.*", "timer", "v1_w", "TIMER"),
    ("EFR32MG26.*:TIMER[89]_NS:.*", "timer", "v1_w", "TIMER"),
    ("EFR32MG2[46].*:TIMER[2-7]_NS:.*", "timer", "v1", "TIMER"),
    // IADC high-accuracy variant. Some sub-families ship an IADC with an
    // extra `OSRHA` field plus HIGHACCURACY / HIGHSPEED ADCMODE enum
    // variants; others don't. All report <version>3</version>. The HA
    // sub-families differ per chip family:
    //   MG24: hundreds digit 1 or 3 (A1xx/B1xx/A3xx/B3xx).
    //   MG26: hundreds digit 3 or 5 (B3xx/B5xx).
    (
        "EFR32MG24[A-Z][13][0-9][0-9].*:IADC[0-9]+_NS:.*",
        "iadc",
        "v3_ha",
        "IADC",
    ),
    (
        "EFR32MG26[A-Z][35][0-9][0-9].*:IADC[0-9]+_NS:.*",
        "iadc",
        "v3_ha",
        "IADC",
    ),
    // SMU MVP-aware variant. Chips that include the MVP peripheral add
    // MVPAHBDATA0..2 fields and an MVP privilege/secure-access bit to
    // SMU's access-control registers. We enumerate the chip patterns
    // that ship MVP (see `grep MVP_NS` across each pack):
    //
    //   MG24: B210 / B220 / B310
    //   MG26: B410 / B420 / B510 / B520 / B610  (all last-digit-0 in 4xx-6xx)
    //
    // SMU version stays v3 on MG24, v7 on MG26 — the divergence is the
    // chip's MVP presence, not the SMU IP version.
    ("EFR32MG24B[23][0-9][0-9]F.*:SMU_NS:.*", "smu", "v3_mvp", "SMU"),
    ("EFR32MG26B[456][0-9]0F.*:SMU_NS:.*", "smu", "v7_mvp", "SMU"),
    // SYSCFG MVP-aware variant. Same chip set as SMU above adds
    // MVPAHBDATA0/1/2 PORTSEL fields to SYSCFG's port-select register.
    ("EFR32MG24B[23][0-9][0-9]F.*:SYSCFG_NS:.*", "syscfg", "v3_mvp", "SYSCFG"),
    ("EFR32MG26B[456][0-9]0F.*:SYSCFG_NS:.*", "syscfg", "v9_mvp", "SYSCFG"),
    // SMU_NS_CFGNS / SMU_S_CFGNS MVP-aware variants. Same chip set as SMU.
    (
        "EFR32MG24B[23][0-9][0-9]F.*:SMU_NS_CFGNS:.*",
        "smu_ns_cfgns",
        "v3_mvp",
        "SMU_NS_CFGNS",
    ),
    (
        "EFR32MG24B[23][0-9][0-9]F.*:SMU_S_CFGNS:.*",
        "smu_s_cfgns",
        "v3_mvp",
        "SMU_S_CFGNS",
    ),
    (
        "EFR32MG26B[456][0-9]0F.*:SMU_NS_CFGNS:.*",
        "smu_ns_cfgns",
        "v7_mvp",
        "SMU_NS_CFGNS",
    ),
    (
        "EFR32MG26B[456][0-9]0F.*:SMU_S_CFGNS:.*",
        "smu_s_cfgns",
        "v7_mvp",
        "SMU_S_CFGNS",
    ),
    // DMEM wait-states variant. MG24 has a single DMEM_NS instance that
    // exposes a CTRL.WAITSTATES bit (RAM read wait-states). MG26 has two
    // DMEM instances (DMEM0_NS, DMEM1_NS) without that field. Both report
    // <version>2</version>; the WAITSTATES bit is the only IR difference.
    ("EFR32MG24.*:DMEM_NS:.*", "dmem", "v2_ws", "DMEM"),
    // DEVINFO is a per-family factory-programmed block. Both families
    // report <version>0.0</version> but their register layouts differ
    // substantially (different calibration data, chip-specific fields).
    // Route to family-specific versions.
    ("EFR32MG24.*:DEVINFO:.*", "devinfo", "v0_mg24", "DEVINFO"),
    ("EFR32MG26.*:DEVINFO:.*", "devinfo", "v0_mg26", "DEVINFO"),
];

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
/// - version: `v<svd_version>` (where `svd_version` is the SVD tag, or
///   `unknown` if the SVD doesn't carry one).
fn default_route(peripheral: &str, svd_version: Option<&str>) -> Route {
    let stripped = peripheral
        .strip_suffix("_NS")
        .or_else(|| peripheral.strip_suffix("_S"))
        .unwrap_or(peripheral);
    let trimmed = stripped.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = if trimmed.is_empty() { stripped } else { trimmed };
    let kind = base.to_ascii_lowercase();
    let version = match svd_version {
        Some(v) if !v.is_empty() => format!("v{}", sanitise_version(v)),
        _ => "vunknown".to_owned(),
    };
    Route {
        kind,
        version,
        block: base.to_owned(),
    }
}

/// Route a peripheral instance to its `(kind, version, block)`. The
/// `compiled` argument should come from [`compile`].
pub fn route(compiled: &[Entry], chip: &str, peripheral: &str, svd_version: Option<&str>) -> Route {
    let key = format!("{chip}:{peripheral}:{}", svd_version.unwrap_or(""));
    for e in compiled {
        if e.key.is_match(&key) {
            return Route {
                kind: e.kind.to_owned(),
                version: e.version.to_owned(),
                block: e.block.to_owned(),
            };
        }
    }
    default_route(peripheral, svd_version)
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
        let r = default_route("EUSART0_NS", Some("2"));
        assert_eq!(r.kind, "eusart");
        assert_eq!(r.version, "v2");
        assert_eq!(r.block, "EUSART");
    }

    #[test]
    fn default_handles_no_suffix() {
        let r = default_route("CMU", Some("3"));
        assert_eq!(r.kind, "cmu");
        assert_eq!(r.version, "v3");
        assert_eq!(r.block, "CMU");
    }

    #[test]
    fn default_falls_back_when_no_svd_version() {
        let r = default_route("GPIO_NS", None);
        assert_eq!(r.version, "vunknown");
    }

    #[test]
    fn empty_perimap_uses_default_for_known_silabs_names() {
        let compiled = compile().unwrap();
        let r = route(&compiled, "EFR32MG26B211F2048IM68", "GPIO_NS", Some("7"));
        assert_eq!(r.kind, "gpio");
        assert_eq!(r.version, "v7");
        assert_eq!(r.block, "GPIO");
    }
}
