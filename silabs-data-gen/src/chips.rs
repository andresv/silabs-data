use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::header::{HeaderDmaRequest, HeaderIrq};
use crate::interrupts::PeripheralInterrupt;
use crate::pdsc::Chip;
use crate::perimap::{self, Entry};
use crate::svd::PeripheralIr;

#[derive(Serialize, Deserialize)]
pub struct ChipFile {
    pub chip: Chip,
    pub peripherals: Vec<PeripheralInstance>,
    pub interrupts: Vec<Interrupt>,
    /// Bonded GPIO pins, sorted by port, then pin. Empty on Series 0: its
    /// headers carry no pin masks.
    #[serde(default)]
    pub pins: Vec<Pin>,
    /// Number of DMA channels. 0 when the chip has no DMA.
    #[serde(default)]
    pub dma_channel_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pin {
    pub port: u8,
    pub pin: u8,
}

/// One DMA request signal of a peripheral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeripheralDmaRequest {
    /// Signal name without the peripheral prefix (`RXFL`, `CC0`).
    pub signal: String,
    pub sourcesel: u8,
    pub sigsel: u8,
}

/// Data from the CMSIS headers of one chip.
#[derive(Debug, Default)]
pub struct HeaderData {
    pub irqs: Vec<HeaderIrq>,
    /// `(port index, bonded pin mask)`.
    pub gpio_port_masks: Vec<(u8, u32)>,
    pub dma_requests: Vec<HeaderDmaRequest>,
    pub dma_channel_count: u8,
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
    /// IRQs of the peripheral. Empty on a secure alias whose `_NS` peer
    /// exists: the peer owns them.
    #[serde(default)]
    pub interrupts: Vec<PeripheralInterrupt>,
    /// DMA request signals, with the same alias rule as `interrupts`.
    #[serde(default)]
    pub dma_requests: Vec<PeripheralDmaRequest>,
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
    header: &HeaderData,
    perimap_entries: &[Entry],
) -> Result<ChipFile> {
    let series = chip
        .series
        .as_ref()
        .with_context(|| format!("{}: series must be set before routing peripherals", chip.name))?
        .series;
    let interrupts = build_interrupts(&header.irqs);

    let canonical = canonical_names(peripherals);
    let owners: Vec<&str> = canonical.iter().flatten().map(String::as_str).collect();
    let irq_names: Vec<&str> = interrupts.iter().map(|i| i.name.as_str()).collect();
    let mut irqs = crate::interrupts::attach(&owners, &irq_names);

    let mut dma: BTreeMap<&str, Vec<PeripheralDmaRequest>> = BTreeMap::new();
    let mut skipped: BTreeSet<&str> = BTreeSet::new();
    for r in &header.dma_requests {
        if owners.contains(&r.source.as_str()) {
            dma.entry(&r.source).or_default().push(PeripheralDmaRequest {
                signal: r.signal.clone(),
                sourcesel: r.sourcesel,
                sigsel: r.sigsel,
            });
        } else {
            skipped.insert(&r.source);
        }
    }
    if !skipped.is_empty() {
        eprintln!("{}: DMA sources with no peripheral: {skipped:?}", chip.name);
    }

    let instances = peripherals
        .iter()
        .zip(&canonical)
        .map(|(p, canonical)| {
            let route = perimap::route(perimap_entries, &chip.name, &p.name, p.version.as_deref(), series)?;
            Ok(PeripheralInstance {
                name: p.name.clone(),
                base_address: p.base_address,
                version: p.version.clone(),
                kind: route.kind,
                register_version: route.version,
                block: route.block,
                interrupts: canonical.as_ref().and_then(|c| irqs.remove(c)).unwrap_or_default(),
                dma_requests: canonical.as_deref().and_then(|c| dma.remove(c)).unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut pins: Vec<Pin> = header
        .gpio_port_masks
        .iter()
        .flat_map(|&(port, mask)| {
            (0..32)
                .filter(move |b| mask & (1 << b) != 0)
                .map(move |pin| Pin { port, pin })
        })
        .collect();
    pins.sort();

    Ok(ChipFile {
        chip,
        peripherals: instances,
        interrupts,
        pins,
        dma_channel_count: header.dma_channel_count,
    })
}

/// Canonical name of each SVD instance: the name without a trailing `_NS`.
/// `None` for a secure alias whose `_NS` peer exists, because the peer owns
/// the metadata row.
fn canonical_names(peripherals: &[PeripheralIr]) -> Vec<Option<String>> {
    let names: BTreeSet<&str> = peripherals.iter().map(|p| p.name.as_str()).collect();
    peripherals
        .iter()
        .map(|p| {
            if secure_to_nonsecure_name(&p.name).is_some_and(|peer| names.contains(peer.as_str())) {
                return None;
            }
            Some(p.name.strip_suffix("_NS").unwrap_or(&p.name).to_owned())
        })
        .collect()
}

/// Return the corresponding non-secure alias name for an `_S`/`_S_` name.
pub fn secure_to_nonsecure_name(name: &str) -> Option<String> {
    if let Some(base) = name.strip_suffix("_S") {
        return Some(format!("{base}_NS"));
    }
    name.find("_S_").map(|at| {
        let mut peer = name.to_owned();
        peer.replace_range(at..at + 3, "_NS_");
        peer
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
            flash_page_size: Some(0x2000),
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

        let cf = build(fake_chip("FAKE"), &peripherals, &HeaderData::default(), &entries).unwrap();

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
