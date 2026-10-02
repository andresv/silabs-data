//! Series 2 USERDATA: no Series 2 SVD lists the user data page, so
//! `data/extra_peripherals.yaml` adds it, and its register YAML is written by
//! hand from `sl_token_manager_manufacturing.h` (Zlib).
//!
//! The test reads the committed data files. It checks that a Series 2 chip
//! gets a `USERDATA` peripheral at the header's base address, routed to
//! `userdata_s2v1`, and that the YAML has the tokens the HAL reads.

use std::path::PathBuf;

use silabs_data_gen::chips::{HeaderData, build};
use silabs_data_gen::header::Series;
use silabs_data_gen::pdsc::Chip;
use silabs_data_gen::svd::PeripheralIr;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The base-address lines of `efr32mg24b220f1536im48.h` (Zlib) that the test needs.
const HEADER: &str = "\
#define CMU_S_BASE                                        (0x40008000UL) /* CMU_S base address */
#define USERDATA_BASE                                     (0x0FE00000UL) /** USERDATA base address */
";

fn mg24_chip() -> Chip {
    Chip {
        name: "EFR32MG24B220F1536IM48".into(),
        family: "EFR32MG24".into(),
        core: "Cortex-M33".into(),
        fpu: true,
        mpu: true,
        trustzone: true,
        series: Some(Series { series: 2, config: 4 }),
        nvic_prio_bits: Some(4),
        flash_page_size: Some(8192),
        memory: vec![],
        flash_algo: None,
        svd: "SVD/EFR32MG24/EFR32MG24B220F1536IM48.svd".into(),
        package: None,
    }
}

#[test]
fn series2_chips_get_userdata() {
    let extra = silabs_data_gen::extra::load(&repo().join("data/extra_peripherals.yaml")).unwrap();
    for family in ["EFR32MG22", "EFR32MG24", "EFR32MG26", "EFR32FG25"] {
        assert_eq!(extra[family], ["USERDATA"], "{family}");
    }

    let mut peripherals = vec![PeripheralIr {
        name: "CMU_S".into(),
        base_address: 0x4000_8000,
        version: Some("3".into()),
        registers: vec![],
        fingerprint: String::new(),
    }];
    silabs_data_gen::extra::append(&extra, "EFR32MG24", "EFR32MG24B220F1536IM48", HEADER, &mut peripherals).unwrap();
    let perimap = silabs_data_gen::perimap::compile().unwrap();
    let chip = build(mg24_chip(), &peripherals, &HeaderData::default(), &perimap).unwrap();

    let userdata = chip.peripherals.iter().find(|p| p.name == "USERDATA").unwrap();
    assert_eq!(userdata.base_address, 0x0FE0_0000);
    assert_eq!(
        (
            userdata.kind.as_str(),
            userdata.register_version.as_str(),
            userdata.block.as_str()
        ),
        ("userdata", "s2v1", "USERDATA")
    );
}

#[test]
fn userdata_yaml_has_the_token_layout() {
    let text = std::fs::read_to_string(repo().join("data/registers/userdata_s2v1.yaml")).unwrap();
    let ir: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    let item = |name: &str| -> (u64, u64, String) {
        let i = ir["block/Userdata"]["items"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("no register {name}"))
            .clone();
        (
            i["byte_offset"].as_u64().unwrap(),
            i["bit_size"].as_u64().unwrap(),
            i["access"].as_str().unwrap().to_owned(),
        )
    };
    // `TOKEN_MFG_*` offsets and `TOKEN_MFG_*_SIZE` values.
    assert_eq!(item("mfg_lfxo_tune"), (0x09C, 8, "Read".into()));
    assert_eq!(item("mfg_ctune"), (0x100, 16, "Read".into()));
}
