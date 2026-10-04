//! Peripherals that a family's SVD does not list, from
//! `data/extra_peripherals.yaml`.
//!
//! The EFM32GG SVD has no `DEVINFO` and no `ROMTABLE`, but the device header
//! defines both (`DEVINFO_BASE`, `ROMTABLE_BASE`) and the family has a Zlib
//! header with their register layout. The file holds names only, because the
//! base address comes from the chip's device header. Each name needs a register
//! YAML `data/registers/<kind>_<version>.yaml`, as for any other peripheral.
//!
//! The file maps the exact chip `family` to peripheral names:
//!
//! ```yaml
//! EFM32GG:
//!   - DEVINFO
//!   - ROMTABLE
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::svd::PeripheralIr;

/// Extra peripheral names for each family.
pub type ExtraPeripherals = BTreeMap<String, Vec<String>>;

/// Read `path`. A missing file gives no extra peripherals.
pub fn load(path: &Path) -> Result<ExtraPeripherals> {
    if !path.is_file() {
        return Ok(ExtraPeripherals::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

/// Parse the file text.
pub fn parse(text: &str) -> Result<ExtraPeripherals> {
    let extra: ExtraPeripherals = serde_yaml::from_str(text)?;
    for (family, names) in &extra {
        for name in names {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                bail!("{family}: `{name}` is not an upper-case peripheral name");
            }
        }
    }
    Ok(extra)
}

/// The `#define <name>_BASE (0x...UL)` value of a device header.
fn header_base(header: &str, name: &str) -> Option<u64> {
    let re = Regex::new(&format!(r"(?m)^#define\s+{name}_BASE\s+\(?\s*0x([0-9A-Fa-f]+)UL\s*\)?"))
        .expect("base-address regex compiles");
    re.captures(header).and_then(|c| u64::from_str_radix(&c[1], 16).ok())
}

/// Append the extra peripherals of `family` to `peripherals`. Each base
/// address comes from `header`. Stops when the header has no base address
/// for a name, or when the SVD already lists the peripheral.
pub fn append(
    extra: &ExtraPeripherals,
    family: &str,
    chip: &str,
    header: &str,
    peripherals: &mut Vec<PeripheralIr>,
) -> Result<()> {
    let Some(names) = extra.get(family) else {
        return Ok(());
    };
    for name in names {
        if peripherals.iter().any(|p| &p.name == name) {
            bail!("{chip}: the SVD already has `{name}`. Remove it from data/extra_peripherals.yaml.");
        }
        let base_address = header_base(header, name)
            .with_context(|| format!("{chip}: the device header has no `#define {name}_BASE`"))?;
        peripherals.push(PeripheralIr {
            name: name.clone(),
            base_address,
            version: None,
            registers: Vec::new(),
            fingerprint: String::new(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "\
#define CALIBRATE_BASE    (0x0FE08000UL) /**< CALIBRATE base address */
#define DEVINFO_BASE      (0x0FE081B0UL) /**< DEVINFO base address */
#define ROMTABLE_BASE     (0xE00FFFD0UL) /**< ROMTABLE base address */
";

    fn svd(name: &str) -> PeripheralIr {
        PeripheralIr {
            name: name.into(),
            base_address: 0x4000_0000,
            version: None,
            registers: Vec::new(),
            fingerprint: String::new(),
        }
    }

    #[test]
    fn appends_with_the_header_base_address() {
        let extra = parse("EFM32GG:\n  - DEVINFO\n  - ROMTABLE\n").unwrap();
        let mut ps = vec![svd("CMU")];
        append(&extra, "EFM32GG", "EFM32GG390F1024", HEADER, &mut ps).unwrap();
        let got: Vec<(&str, u64)> = ps.iter().map(|p| (p.name.as_str(), p.base_address)).collect();
        assert_eq!(
            got,
            [
                ("CMU", 0x4000_0000),
                ("DEVINFO", 0x0FE0_81B0),
                ("ROMTABLE", 0xE00F_FFD0)
            ]
        );

        // Another family gets nothing.
        let mut ps = vec![svd("CMU")];
        append(&extra, "EFR32MG24", "X", HEADER, &mut ps).unwrap();
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn stops_on_a_missing_base_or_a_duplicate() {
        let extra = parse("EFM32GG:\n  - LOCKBITS\n").unwrap();
        let err = append(&extra, "EFM32GG", "C", HEADER, &mut vec![])
            .unwrap_err()
            .to_string();
        assert!(err.contains("LOCKBITS_BASE"), "{err}");

        let extra = parse("EFM32GG:\n  - DEVINFO\n").unwrap();
        let err = append(&extra, "EFM32GG", "C", HEADER, &mut vec![svd("DEVINFO")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("already has"), "{err}");

        assert!(parse("EFM32GG:\n  - devinfo\n").is_err());
    }
}
