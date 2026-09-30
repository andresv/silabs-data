use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::header::HeaderIrq;
use crate::pdsc::Chip;
use crate::perimap::{self, Entry};
use crate::svd::PeripheralIr;

#[derive(Serialize, Deserialize)]
pub struct ChipFile {
    pub chip: Chip,
    pub peripherals: Vec<PeripheralInstance>,
    pub interrupts: Vec<Interrupt>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PeripheralInstance {
    /// Full SVD name, including `_NS`/`_S` suffix.
    pub name: String,
    pub base_address: u64,
    /// Peripheral version from SVD `<peripheral><version>` tag, when present.
    pub version: Option<String>,
    /// Canonical kind (lowercase, no `_NS`/`_S` suffix, no trailing digits).
    /// Routed via `perimap`. Example: `gpio`, `eusart`, `timer`.
    pub kind: String,
    /// Routed register-YAML version label, e.g. `s2v3`, `s2v7`, `s0v1`.
    /// Names the `data/registers/<kind>_<version>.yaml` file the peripheral
    /// uses for its register layout.
    pub register_version: String,
    /// Canonical block name inside the register YAML, e.g. `GPIO`, `EUSART`.
    pub block: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interrupt {
    pub name: String,
    pub value: u32,
    pub description: Option<String>,
}

pub fn build(
    chip: Chip,
    peripherals: &[PeripheralIr],
    header_irqs: &[HeaderIrq],
    perimap_entries: &[Entry],
) -> Result<ChipFile> {
    let series = chip
        .series
        .as_ref()
        .with_context(|| format!("{}: series must be set before routing peripherals", chip.name))?
        .series;
    let instances = peripherals
        .iter()
        .map(|p| {
            let route = perimap::route(perimap_entries, &chip.name, &p.name, p.version.as_deref(), series)?;
            Ok(PeripheralInstance {
                name: p.name.clone(),
                base_address: p.base_address,
                version: p.version.clone(),
                kind: route.kind,
                register_version: route.version,
                block: route.block,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ChipFile {
        chip,
        peripherals: instances,
        interrupts: build_interrupts(header_irqs),
    })
}

/// Build the chip's interrupt table from the CMSIS device header (see
/// [`crate::header`] for why not the SVD), sorted by IRQ value.
fn build_interrupts(header: &[HeaderIrq]) -> Vec<Interrupt> {
    let mut out: Vec<Interrupt> = header
        .iter()
        .map(|h| Interrupt {
            name: h.name.clone(),
            value: h.value,
            description: None,
        })
        .collect();
    out.sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdsc::Chip;

    fn fake_chip(name: &str) -> Chip {
        Chip {
            name: name.to_string(),
            core: "CM33".to_string(),
            fpu: true,
            mpu: true,
            trustzone: true,
            series: Some(crate::header::Series { series: 2, config: 6 }),
            nvic_prio_bits: Some(4),
            memory: vec![],
            flash_algo: None,
            svd: "fake.svd".to_string(),
            package: None,
        }
    }

    /// `build()` copies the routed `(kind, version, block)` of each `Entry`
    /// into its `PeripheralInstance`, and the result round-trips through JSON.
    ///
    /// A hand-made `Entry` list keeps this test stable when the real perimap
    /// entries change. The `perimap.rs` tests cover the real entries.
    #[test]
    fn build_threads_routed_kind_version_block_into_json() {
        use regex::Regex;

        use crate::perimap::Entry;

        let entries = vec![Entry {
            key: Regex::new("^FAKE:FOO_NS:1$").unwrap(),
            kind: "foo",
            version: "v1_custom",
            block: "FooBlock",
        }];

        let peripherals = vec![
            // Matches the custom Entry above — should pick up "foo"/"v1_custom"/"FooBlock".
            PeripheralIr {
                name: "FOO_NS".to_string(),
                base_address: 0x1000_0000,
                version: Some("1".to_string()),
                registers: vec![],
                fingerprint: "deadbeef".repeat(8),
            },
            // Matches no Entry — should fall through to `default_route`.
            PeripheralIr {
                name: "BAR0_NS".to_string(),
                base_address: 0x2000_0000,
                version: Some("3".to_string()),
                registers: vec![],
                fingerprint: "feedface".repeat(8),
            },
        ];

        let cf = build(fake_chip("FAKE"), &peripherals, &[], &entries).unwrap();

        assert_eq!(cf.peripherals.len(), 2);

        // Routed via the custom Entry.
        assert_eq!(cf.peripherals[0].kind, "foo");
        assert_eq!(cf.peripherals[0].register_version, "v1_custom");
        assert_eq!(cf.peripherals[0].block, "FooBlock");

        // Routed via default — strip `_NS`, strip trailing digit, lowercase kind,
        // prepend `s<series>v` to the SVD version, block name without suffix.
        assert_eq!(cf.peripherals[1].kind, "bar");
        assert_eq!(cf.peripherals[1].register_version, "s2v3");
        assert_eq!(cf.peripherals[1].block, "BAR");

        // JSON round-trip.
        let json = serde_json::to_string(&cf).unwrap();
        let back: ChipFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peripherals[0].kind, "foo");
        assert_eq!(back.peripherals[0].block, "FooBlock");
        assert_eq!(back.peripherals[1].register_version, "s2v3");
    }

    /// The header gives the interrupt table verbatim. Sorting by IRQ value
    /// keeps `device.x` deterministic.
    #[test]
    fn build_interrupts_uses_header_verbatim_sorted_by_value() {
        let header = vec![
            HeaderIrq {
                name: "MODEM".into(),
                value: 50,
            },
            HeaderIrq {
                name: "TIMER0".into(),
                value: 4,
            },
            HeaderIrq {
                name: "FRC".into(),
                value: 49,
            },
            HeaderIrq {
                name: "SMU_SECURE".into(),
                value: 0,
            },
        ];

        let irqs = build_interrupts(&header);

        let names: Vec<&str> = irqs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["SMU_SECURE", "TIMER0", "FRC", "MODEM"]);

        let values: Vec<u32> = irqs.iter().map(|i| i.value).collect();
        assert_eq!(values, [0, 4, 49, 50]);
    }
}
