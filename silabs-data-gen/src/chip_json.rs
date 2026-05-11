use crate::pdsc::Chip;
use crate::perimap::{self, Entry};
use crate::svd::{InterruptIr, PeripheralIr};
use serde::{Deserialize, Serialize};

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
    /// Routed register-YAML version label, e.g. `v3`, `v7`, `v2_lf`.
    /// Names the `data/registers/<kind>_<version>.yaml` file the peripheral
    /// uses for its register layout.
    pub register_version: String,
    /// Canonical block name inside the register YAML, e.g. `GPIO`, `EUSART`.
    pub block: String,
}

#[derive(Serialize, Deserialize)]
pub struct Interrupt {
    pub name: String,
    pub value: u32,
    pub description: Option<String>,
}

pub fn build(
    chip: Chip,
    peripherals: &[PeripheralIr],
    interrupts: &[InterruptIr],
    perimap_entries: &[Entry],
) -> ChipFile {
    let instances = peripherals
        .iter()
        .map(|p| {
            let route = perimap::route(
                perimap_entries,
                &chip.name,
                &p.name,
                p.version.as_deref(),
            );
            PeripheralInstance {
                name: p.name.clone(),
                base_address: p.base_address,
                version: p.version.clone(),
                kind: route.kind,
                register_version: route.version,
                block: route.block,
            }
        })
        .collect();

    let ints = interrupts
        .iter()
        .map(|i| Interrupt {
            name: i.name.clone(),
            value: i.value,
            description: i.description.clone(),
        })
        .collect();

    ChipFile {
        chip,
        peripherals: instances,
        interrupts: ints,
    }
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
            memory: vec![],
            flash_algo: None,
            svd: "fake.svd".to_string(),
            package: None,
        }
    }

    /// Verify that `build()` threads the perimap-routed `(kind, version,
    /// block)` triple from each `Entry` into the corresponding
    /// `PeripheralInstance` and that the result round-trips through JSON.
    ///
    /// Uses a hand-rolled minimal `Entry` list (not `perimap::compile()`),
    /// so adding real perimap entries doesn't churn this test. The actual
    /// routing semantics of real entries are covered by `perimap.rs` tests.
    #[test]
    fn build_threads_routed_kind_version_block_into_json() {
        use crate::perimap::Entry;
        use regex::Regex;

        let entries = vec![
            Entry {
                key: Regex::new("^FAKE:FOO_NS:1$").unwrap(),
                kind: "foo",
                version: "v1_custom",
                block: "FooBlock",
            },
        ];

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

        let cf = build(fake_chip("FAKE"), &peripherals, &[], &entries);

        assert_eq!(cf.peripherals.len(), 2);

        // Routed via the custom Entry.
        assert_eq!(cf.peripherals[0].kind, "foo");
        assert_eq!(cf.peripherals[0].register_version, "v1_custom");
        assert_eq!(cf.peripherals[0].block, "FooBlock");

        // Routed via default — strip `_NS`, strip trailing digit, lowercase kind,
        // prepend `v` to SVD version, block name without suffix.
        assert_eq!(cf.peripherals[1].kind, "bar");
        assert_eq!(cf.peripherals[1].register_version, "v3");
        assert_eq!(cf.peripherals[1].block, "BAR");

        // JSON round-trip.
        let json = serde_json::to_string(&cf).unwrap();
        let back: ChipFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peripherals[0].kind, "foo");
        assert_eq!(back.peripherals[0].block, "FooBlock");
        assert_eq!(back.peripherals[1].register_version, "v3");
    }
}
