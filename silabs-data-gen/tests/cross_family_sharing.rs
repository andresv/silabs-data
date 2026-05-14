//! Test pinning the cross-family register-block sharing assumption that
//! drives Phase 3-4 of the MG24 bootstrap plan: peripherals identified by
//! `(name, version)` should have matching SVD register layouts (and hence
//! matching fingerprints), allowing the codegen to emit a single shared
//! Rust block for both families.
//!
//! Fixtures `mg24_subset.svd` / `mg26_subset.svd` are hand-built to mimic the
//! real EFR32MG24 vs EFR32MG26 ground truth from the 2025.12.1 SVDs:
//!   - 10 shared kinds: ACMP0, BURTC, EUSART0, TIMER0, USART0, IADC0, LDMA,
//!     LETIMER0, RTCC, WDOG0
//!     (matching version + matching register layout → matching fingerprint).
//!   - 3 split kinds: GPIO (v3 vs v7), CMU (v3 vs v7), MSC (v3 vs v9)
//!     (different version + different register layout → different fingerprint).
//!
//! That's a 76.9%/23.1% shared/split split — measured share ratio in the
//! real SVDs was 79.5%, so 75% is the assertion threshold.

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
        // Functional consistency: version-equality iff fingerprint-equality.
        // If this ever breaks, Silabs has shipped two SVDs that disagree on a
        // peripheral's register layout while keeping the version tag identical
        // (or vice versa) — Phases 3-4's keyed-by-version sharing strategy
        // would break silently, so fail loudly here.
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
