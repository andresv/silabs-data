//! Pin canonical EFR32MG26 peripheral versions via a hermetic SVD fixture.
//!
//! Values come from Silabs's pdsc/SVD `<peripheral><version>` tag
//! (specifically `EFR32MG26B211F2048IM68.svd`, 2025.12.1 pack). They double as a
//! regression catch — if Silabs ever bumps a peripheral's version without
//! adjusting the rest of the SVD consistently we'll see it reflected here.
//!
//! Hermetic fixture form (mirrors the `eusart0_min.svd` test pattern) keeps
//! this independent of `build/` state.

use silabs_data_gen::svd;
use std::collections::HashMap;

#[test]
fn version_field_round_trips_from_svd_to_peripheral_ir() {
    let xml = include_str!("fixtures/peripheral_versions.svd");
    let ps = svd::parse(xml).expect("parses");
    let by_name: HashMap<&str, Option<&str>> = ps
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_deref()))
        .collect();
    assert_eq!(by_name.get("ACMP0_NS"), Some(&Some("2")));
    assert_eq!(by_name.get("CMU_NS"), Some(&Some("7")));
    assert_eq!(by_name.get("GPIO_NS"), Some(&Some("7")));
    assert_eq!(by_name.get("EUSART0_NS"), Some(&Some("2")));
    assert_eq!(by_name.get("MSC_NS"), Some(&Some("9")));
}
