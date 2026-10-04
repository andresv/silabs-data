//! Peripherals with the same `(name, version)` on two families have the same
//! SVD register layout, and so the same fingerprint. Codegen relies on this
//! to emit one shared Rust block for both families.
//!
//! The fixtures `mg24_subset.svd` / `mg26_subset.svd` are hand-built from the
//! real EFR32MG24 and EFR32MG26 2025.12.1 SVDs:
//!   - 10 shared kinds: ACMP0, BURTC, EUSART0, TIMER0, USART0, IADC0, LDMA,
//!     LETIMER0, RTCC, WDOG0
//!     (same version and register layout → same fingerprint).
//!   - 3 split kinds: GPIO (v3 vs v7), CMU (v3 vs v7), MSC (v3 vs v9)
//!     (different version and register layout → different fingerprint).
//!
//! The fixtures share 76.9% of the kinds. The real SVDs share 79.5%, so the
//! threshold is 75%.

use std::collections::HashMap;

use silabs_data_gen::svd::{self, PeripheralIr};

fn parse_subset(xml: &str) -> HashMap<String, PeripheralIr> {
    svd::parse(xml)
        .expect("fixture parses")
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect()
}

#[test]
fn cross_family_versions_and_fingerprints_are_consistent() {
    let mg24 = parse_subset(include_str!("fixtures/mg24_subset.svd"));
    let mg26 = parse_subset(include_str!("fixtures/mg26_subset.svd"));

    let shared: Vec<&String> = mg24.keys().filter(|k| mg26.contains_key(*k)).collect();
    assert!(!shared.is_empty(), "no shared kinds; fixtures broken");

    let mut version_match = 0usize;
    let mut fingerprint_match = 0usize;
    let mut both_or_neither = 0usize;
    for kind in &shared {
        let a = &mg24[kind.as_str()];
        let b = &mg26[kind.as_str()];
        let v_eq = a.version == b.version;
        let f_eq = a.fingerprint == b.fingerprint;
        if v_eq {
            version_match += 1;
        }
        if f_eq {
            fingerprint_match += 1;
        }
        // Same version if and only if same fingerprint. Otherwise two SVDs
        // disagree on a layout under one version tag (or the reverse), and
        // version-keyed sharing would break silently.
        if v_eq == f_eq {
            both_or_neither += 1;
        }
    }
    let pct = (fingerprint_match * 100) / shared.len();
    assert!(
        pct >= 75,
        "only {pct}% of shared kinds have matching fingerprint (threshold 75%); \
         shared={}, version_match={version_match}, fingerprint_match={fingerprint_match}",
        shared.len()
    );
    assert_eq!(
        both_or_neither,
        shared.len(),
        "(kind, version) does not functionally determine fingerprint — \
         Silabs SVD inconsistency or test fixture drift"
    );
}
