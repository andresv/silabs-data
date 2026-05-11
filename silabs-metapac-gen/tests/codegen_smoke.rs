//! End-to-end smoke test: render an EFR32MG26-shaped SVD via chiptool.
//!
//! Uses a hand-crafted fixture (`tests/fixtures/mg26_smoke.svd`) so the test
//! is hermetic — no dependency on the vendored pack being extracted, no
//! absolute paths. The fixture's numeric values are taken verbatim from
//! `EFR32MG26B211F2048IM68.svd` in DFP `2025.12.1`, so it exercises Silabs's
//! actual NS/S address convention (NS=0x5xxx_xxxx, S=0x4xxx_xxxx).

use silabs_metapac_gen::codegen::{GenerateInput, generate};
use silabs_metapac_gen::extract::extract_ip;
use silabs_metapac_gen::peripheral::strip_secure_peripherals;
use std::path::PathBuf;
use svd_parser::ValidateLevel;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_svd() -> PathBuf {
    manifest_dir().join("tests/fixtures/mg26_smoke.svd")
}

/// Path to repo-root `transforms/` directory.
fn transforms_dir() -> PathBuf {
    manifest_dir().join("../transforms")
}

#[test]
fn renders_mg26_shaped_svd() {
    let svd = fixture_svd();
    let out = generate(GenerateInput {
        svd_path: &svd,
        transforms: &[],
    })
    .expect("generate");

    // Sanity bounds — the fixture is small but produces non-trivial output.
    assert!(
        out.lib_rs.len() > 1_000,
        "lib.rs surprisingly small ({} bytes)",
        out.lib_rs.len()
    );
    // device.x must contain the EUSART0 interrupt PROVIDE line.
    assert!(
        out.device_x.contains("PROVIDE("),
        "device.x missing PROVIDE: {}",
        out.device_x
    );
    assert!(
        out.device_x.contains("EUSART0_RX"),
        "device.x missing the EUSART0_RX interrupt symbol: {}",
        out.device_x
    );

    // NS peripherals make it through chiptool.
    assert!(out.lib_rs.contains("EUSART0"), "EUSART0 missing from lib.rs");
    assert!(out.lib_rs.contains("GPIO"), "GPIO missing from lib.rs");

    // The `_S` (secure-alias) peripherals are stripped before chiptool sees
    // them, so chiptool's IR (and the rendered Rust) must NOT carry them.
    // Match an underscore-prefixed `_S` token to avoid catching e.g. `_STATUS`.
    let leaks: Vec<_> = out
        .lib_rs
        .lines()
        .filter(|l| l.contains("EUSART0_S") || l.contains("GPIO_S"))
        .take(3)
        .collect();
    assert!(
        leaks.is_empty(),
        "_S peripheral leaked into lib.rs:\n{}",
        leaks.join("\n")
    );
}

/// End-to-end check that the per-kind transforms directory loads + applies
/// correctly via the new per-IP extraction path. Uses a hand-crafted
/// GPIO-only fixture so we can pin the post-transform `P_DOUT` array
/// without depending on the full vendored MG24/MG26 SVDs.
#[test]
fn gpio_array_recovered_from_real_transforms() {
    let svd_path = manifest_dir().join("tests/fixtures/gpio_smoke.svd");
    let raw = std::fs::read_to_string(&svd_path).unwrap();
    let stripped = strip_secure_peripherals(&raw).unwrap();
    let cfg = svd_parser::Config::default()
        .expand_properties(true)
        .validate_level(ValidateLevel::Disabled);
    let device = svd_parser::parse_with_config(&stripped, &cfg).unwrap();
    let gpio = device
        .peripherals
        .iter()
        .find(|p| p.name == "GPIO_NS")
        .expect("GPIO_NS peripheral");
    let version = format!(
        "v{}",
        gpio.version.clone().unwrap_or_else(|| "unknown".to_string())
    );
    // The new perimap-driven pipeline routes GPIO_NS → block "GPIO". Pass that
    // directly here; the test doesn't need the perimap itself for this single
    // peripheral.
    let ir = extract_ip(gpio, "GPIO", &version, &transforms_dir())
        .expect("extract_ip");

    // After array recovery, the GPIO block carries a `P_DOUT` array item
    // instead of separate PORTA_DOUT/PORTB_DOUT/... entries.
    let block = ir.blocks.get("GPIO").expect("GPIO block");
    let names: Vec<&str> = block.items.iter().map(|i| i.name.as_str()).collect();
    assert!(
        names.contains(&"P_DOUT"),
        "P_DOUT array missing; block items: {:?}",
        names
    );
    for forbidden in ["PORTA_DOUT", "PORTB_DOUT", "PORTC_DOUT", "PORTD_DOUT"] {
        assert!(
            !names.contains(&forbidden),
            "raw port register `{forbidden}` leaked through array recovery: {:?}",
            names
        );
    }
    // The merged fieldset should be `regs::PORT_DOUT` (BlockWithRegsVals
    // namespace; the outer `Gpio::` prefix is stripped post-extract).
    assert!(
        ir.fieldsets.contains_key("regs::PORT_DOUT"),
        "merged PORT_DOUT fieldset missing; fieldsets: {:?}",
        ir.fieldsets.keys().collect::<Vec<_>>()
    );
}
