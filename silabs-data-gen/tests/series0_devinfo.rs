//! Series 0 DEVINFO and ROMTABLE: the EFM32GG SVD has neither block, so
//! `data/extra_peripherals.yaml` adds them, and their register YAMLs are
//! written by hand from `efm32gg_devinfo.h` and `efm32gg_romtable.h`.
//!
//! The test reads the committed data files. It checks that a Series 0 chip
//! gets a `DEVINFO` peripheral at the header's base address, routed to
//! `devinfo_s0v1`, and that the YAML has the fields the HAL reads.

use std::path::PathBuf;

use silabs_data_gen::chips::{HeaderData, build};
use silabs_data_gen::header::Series;
use silabs_data_gen::pdsc::Chip;
use silabs_data_gen::svd::PeripheralIr;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The base-address lines of `efm32gg390f1024.h` (Zlib) that the test needs.
const HEADER: &str = "\
#define CMU_BASE          (0x400C8000UL) /**< CMU base address  */
#define DEVINFO_BASE      (0x0FE081B0UL) /**< DEVINFO base address */
#define ROMTABLE_BASE     (0xE00FFFD0UL) /**< ROMTABLE base address */
";

fn efm32gg_chip() -> Chip {
    Chip {
        name: "EFM32GG390F1024".into(),
        family: "EFM32GG".into(),
        core: "Cortex-M3".into(),
        fpu: false,
        mpu: true,
        trustzone: false,
        series: Some(Series { series: 0, config: 0 }),
        nvic_prio_bits: Some(3),
        flash_page_size: Some(4096),
        memory: vec![],
        flash_algo: None,
        svd: "SVD/EFM32GG/EFM32GG390F1024.svd".into(),
        package: None,
    }
}

#[test]
fn series0_chip_gets_devinfo_and_romtable() {
    let extra = silabs_data_gen::extra::load(&repo().join("data/extra_peripherals.yaml")).unwrap();
    let mut peripherals = vec![PeripheralIr {
        name: "CMU".into(),
        base_address: 0x400C_8000,
        version: None,
        registers: vec![],
        fingerprint: String::new(),
    }];
    silabs_data_gen::extra::append(&extra, "EFM32GG", "EFM32GG390F1024", HEADER, &mut peripherals).unwrap();
    let perimap = silabs_data_gen::perimap::compile().unwrap();
    let chip = build(efm32gg_chip(), &peripherals, &HeaderData::default(), &perimap).unwrap();

    let find = |name: &str| chip.peripherals.iter().find(|p| p.name == name).unwrap();
    let devinfo = find("DEVINFO");
    assert_eq!(devinfo.base_address, 0x0FE0_81B0);
    assert_eq!(
        (
            devinfo.kind.as_str(),
            devinfo.register_version.as_str(),
            devinfo.block.as_str()
        ),
        ("devinfo", "s0v1", "DEVINFO")
    );
    let romtable = find("ROMTABLE");
    assert_eq!(romtable.base_address, 0xE00F_FFD0);
    assert_eq!(
        (romtable.kind.as_str(), romtable.register_version.as_str()),
        ("romtable", "s0v1")
    );
}

#[test]
fn devinfo_yaml_has_the_header_layout() {
    let text = std::fs::read_to_string(repo().join("data/registers/devinfo_s0v1.yaml")).unwrap();
    let ir: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    let item = |name: &str| -> u64 {
        ir["block/Devinfo"]["items"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("no register {name}"))["byte_offset"]
            .as_u64()
            .unwrap()
    };
    let field = |fieldset: &str, name: &str| -> (u64, u64) {
        let f = ir[format!("fieldset/regs::{fieldset}").as_str()]["fields"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no field {fieldset}.{name}"))
            .clone();
        (f["bit_offset"].as_u64().unwrap(), f["bit_size"].as_u64().unwrap())
    };
    // `DEVINFO_TypeDef` offsets and `_DEVINFO_*_MASK` / `_SHIFT` values.
    assert_eq!(item("hfrcocal0"), 0x2C);
    assert_eq!(item("hfrcocal1"), 0x30);
    assert_eq!(item("uniquel"), 0x40);
    assert_eq!(item("part"), 0x4C);
    assert_eq!(field("Hfrcocal0", "band1"), (0, 8));
    assert_eq!(field("Hfrcocal0", "band14"), (24, 8));
    assert_eq!(field("Hfrcocal1", "band28"), (8, 8));
    assert_eq!(field("Part", "prod_rev"), (24, 8));
    assert_eq!(field("Part", "device_family"), (16, 8));
    assert_eq!(field("Msize", "sram"), (16, 16));
}
