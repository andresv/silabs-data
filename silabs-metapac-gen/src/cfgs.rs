//! Per-chip `cfg` names for crates that depend on `silabs-metapac`.
//!
//! The metapac `build.rs` passes them to direct dependents through Cargo
//! `links` metadata (`DEP_SILABS_METAPAC_CFGS`,
//! `DEP_SILABS_METAPAC_CHECK_CFGS`). A dependent therefore needs no chip
//! features of its own.
//!
//! Names, for a chip with LETIMER0 at version `s0v1`:
//! - `letimer` — the chip has the kind.
//! - `letimer_s0v1` — exact register version.
//! - `letimer_s0` — any Series 0 version.
//! - Every `_`-separated prefix of the label: `devinfo_s2v0_mg24` also gives
//!   `devinfo_s2v0`.
//! - `silabs_series="0"`, and `silabs_series_2_config="4"` for chips
//!   with a config number (Series 1+).
//! - `peri_<name>` per peripheral instance, from the lowercase
//!   `METADATA.peripherals` name: `peri_timer0`, `peri_gpio`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use silabs_data_gen::chips::ChipFile;

use crate::crate_layout::canonical_peripheral_names;

/// `peri_<name>` cfgs of one chip.
fn peri_cfgs(chip: &ChipFile) -> impl Iterator<Item = String> {
    canonical_peripheral_names(chip)
        .into_iter()
        .map(|n| format!("peri_{}", n.to_ascii_lowercase()))
}

/// Cfg names implied by one `(kind, version)` pair.
pub fn version_cfgs(kind: &str, version: &str) -> Vec<String> {
    static SERIES_LABEL: OnceLock<Regex> = OnceLock::new();
    let series_label = SERIES_LABEL.get_or_init(|| Regex::new(r"^(s\d+)v\d").unwrap());

    let mut out = vec![kind.to_owned()];
    if let Some(m) = series_label.captures(version) {
        out.push(format!("{kind}_{}", &m[1]));
    }
    let parts: Vec<&str> = version.split('_').collect();
    for i in 1..=parts.len() {
        out.push(format!("{kind}_{}", parts[..i].join("_")));
    }
    out.dedup();
    out
}

/// `(series, config)` for the `silabs_series*` cfgs. Series 0 has no config.
fn series_of(chip: &ChipFile) -> (u8, Option<u16>) {
    let s = chip
        .chip
        .series
        .expect("chip.series missing — re-run silabs-data-gen to populate it");
    match s.series {
        0 => (0, None),
        n => (n, Some(s.config)),
    }
}

/// All cfgs one chip enables, sorted.
pub fn chip_cfgs(chip: &ChipFile) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let (series, config) = series_of(chip);
    out.insert(format!("silabs_series=\"{series}\""));
    if let Some(config) = config {
        out.insert(format!("silabs_series_{series}_config=\"{config}\""));
    }
    for p in &chip.peripherals {
        out.extend(version_cfgs(&p.kind, &p.register_version));
    }
    out.extend(peri_cfgs(chip));
    out
}

/// Series and config numbers that Silicon Labs defines, with or without
/// chips in the metapac: Series 1 configs 1..4, Series 2 configs 1..9
/// (xG21..xG29), Series 3 config 301. A HAL can then gate code for a family
/// that the metapac does not cover, without `unexpected_cfgs` warnings.
const KNOWN_CONFIGS: &[(u8, &[u16])] = &[
    (0, &[]),
    (1, &[1, 2, 3, 4]),
    (2, &[1, 2, 3, 4, 5, 6, 7, 8, 9]),
    (3, &[301]),
];

/// `rustc-check-cfg` specs that declare every cfg any chip can enable.
/// Specs contain no spaces, so a dependent can split the list on spaces.
pub fn check_cfgs(chips: &[ChipFile]) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut series_values: BTreeSet<u8> = KNOWN_CONFIGS.iter().map(|(s, _)| *s).collect();
    let mut config_values: BTreeMap<u8, BTreeSet<u16>> = KNOWN_CONFIGS
        .iter()
        .filter(|(_, c)| !c.is_empty())
        .map(|(s, c)| (*s, c.iter().copied().collect()))
        .collect();
    for chip in chips {
        let (series, config) = series_of(chip);
        series_values.insert(series);
        if let Some(config) = config {
            config_values.entry(series).or_default().insert(config);
        }
        for p in &chip.peripherals {
            names.extend(version_cfgs(&p.kind, &p.register_version));
        }
        names.extend(peri_cfgs(chip));
    }

    fn values<T: std::fmt::Display>(v: &BTreeSet<T>) -> String {
        v.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(",")
    }

    let mut out: Vec<String> = names.iter().map(|n| format!("cfg({n})")).collect();
    out.push(format!("cfg(silabs_series,values({}))", values(&series_values)));
    for (series, configs) in &config_values {
        out.push(format!(
            "cfg(silabs_series_{series}_config,values({}))",
            values(configs)
        ));
    }
    out
}

/// Write `src/chips/<chip>/cfgs.txt` (one cfg per line), read by the
/// metapac's `build.rs`.
pub fn write_chip_cfgs(chip: &ChipFile, out: &Path) -> Result<()> {
    let mut s = String::new();
    for c in chip_cfgs(chip) {
        s.push_str(&c);
        s.push('\n');
    }
    std::fs::write(out, s).with_context(|| format!("write {}", out.display()))
}

/// Write `src/check_cfgs.txt` (one spec per line), read by the metapac's
/// `build.rs`.
pub fn write_check_cfgs(chips: &[ChipFile], out: &Path) -> Result<()> {
    let mut s = String::new();
    for c in check_cfgs(chips) {
        s.push_str(&c);
        s.push('\n');
    }
    std::fs::write(out, s).with_context(|| format!("write {}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_label_adds_series_prefix() {
        assert_eq!(
            version_cfgs("letimer", "s0v1"),
            ["letimer", "letimer_s0", "letimer_s0v1"]
        );
    }

    #[test]
    fn check_cfgs_declare_known_configs_without_chips() {
        let specs = check_cfgs(&[]);
        assert!(specs.contains(&r#"cfg(silabs_series,values("0","1","2","3"))"#.to_owned()));
        assert!(
            specs.contains(&r#"cfg(silabs_series_2_config,values("1","2","3","4","5","6","7","8","9"))"#.to_owned())
        );
    }

    #[test]
    fn peri_cfgs_use_canonical_names() {
        let peripheral = |name: &str, address: u64, kind: &str| {
            serde_json::json!({ "name": name, "base_address": address, "version": "1",
                                "kind": kind, "register_version": "s2v1", "block": kind.to_uppercase() })
        };
        let chip: ChipFile = serde_json::from_value(serde_json::json!({
            "chip": { "name": "FAKE", "core": "CM33", "fpu": true, "mpu": true, "trustzone": true,
                      "memory": [], "svd": "x.svd", "series": { "series": 2, "config": 4 } },
            "peripherals": [
                peripheral("GPIO_NS", 0x5003_C000, "gpio"),
                peripheral("GPIO_S", 0x4003_C000, "gpio"),
                peripheral("SEMAILBOX_NS_HOST", 0x5C00_0000, "semailbox_ns_host"),
                peripheral("SEMAILBOX_S_HOST", 0x4C00_0000, "semailbox_s_host"),
            ],
            "interrupts": []
        }))
        .unwrap();
        let peri: Vec<String> = chip_cfgs(&chip)
            .into_iter()
            .filter(|c| c.starts_with("peri_"))
            .collect();
        assert_eq!(peri, ["peri_gpio", "peri_semailbox_ns_host"]);
    }

    #[test]
    fn underscore_label_adds_every_prefix() {
        assert_eq!(
            version_cfgs("eusart", "s2v2_lf"),
            ["eusart", "eusart_s2", "eusart_s2v2", "eusart_s2v2_lf"]
        );
        assert_eq!(version_cfgs("timer", "s2v1"), ["timer", "timer_s2", "timer_s2v1"]);
    }
}
