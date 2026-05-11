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

    #[test]
    fn build_routes_peripherals_via_perimap() {
        let peripherals = vec![
            PeripheralIr {
                name: "GPIO_NS".to_string(),
                base_address: 0x5003_C000,
                version: Some("7".to_string()),
                registers: vec![],
                fingerprint: "deadbeef".repeat(8),
            },
            PeripheralIr {
                name: "EUSART0_NS".to_string(),
                base_address: 0x5B01_0000,
                version: Some("2".to_string()),
                registers: vec![],
                fingerprint: "feedface".repeat(8),
            },
        ];
        let entries = perimap::compile().unwrap();
        let cf = build(
            fake_chip("EFR32MG26B211F2048IM68"),
            &peripherals,
            &[],
            &entries,
        );
        assert_eq!(cf.peripherals.len(), 2);
        assert_eq!(cf.peripherals[0].kind, "gpio");
        assert_eq!(cf.peripherals[0].register_version, "v7");
        assert_eq!(cf.peripherals[0].block, "GPIO");
        assert_eq!(cf.peripherals[1].kind, "eusart");
        assert_eq!(cf.peripherals[1].register_version, "v2");
        assert_eq!(cf.peripherals[1].block, "EUSART");

        // Roundtrip via JSON.
        let json = serde_json::to_string(&cf).unwrap();
        let back: ChipFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peripherals[0].kind, "gpio");
        assert_eq!(back.peripherals[1].register_version, "v2");
    }
}
