//! Extract the IRQn enum from a Silicon Labs CMSIS device header.
//!
//! ## Why
//!
//! Silicon Labs CMSIS SVDs do not list radio peripheral IRQs (FRC, MODEM,
//! AGC, BUFC, PROTIMER, SYNTH, RAC_RSM, RAC_SEQ, RFECA0, RFECA1, …) — the
//! peripherals exist but their `<interrupt>` blocks are missing. The
//! per-chip C header `Device/SiliconLabs/<FAMILY>/Include/<chip>.h` has
//! the full IRQ enum:
//!
//! ```c
//! typedef enum IRQn {
//!   SMU_SECURE_IRQn        = 0,
//!   …
//!   FRC_IRQn               = 49,
//!   MODEM_IRQn             = 50,
//!   …
//! } IRQn_Type;
//! ```
//!
//! We parse those lines and use them verbatim as the chip's interrupt
//! table in `chip_json::build`. The SVD's `<interrupt>` blocks are
//! intentionally ignored.
//!
//! This mirrors stm32-data's approach: it doesn't trust the SVD for
//! interrupts either, and parses STM32 HAL headers for `<NAME>_IRQn = N,`
//! enum members (`stm32-data-gen/src/header.rs`).

use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// One IRQ enum entry recovered from the device header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderIrq {
    /// Short name (no `_IRQn` suffix).
    pub name: String,
    /// IRQ number (matches the position in the device NVIC vector table).
    pub value: u32,
}

/// Parse `<NAME>_IRQn = <N>,` enum members from a Silicon Labs CMSIS device
/// header.
///
/// Tolerates extra whitespace, an optional trailing comma, and trailing
/// comments like `/*!<  4 EFR32 TIMER0 Interrupt */`. Skips any negative
/// values (the Cortex-M core exceptions in the header — e.g.
/// `HardFault_IRQn = -13` — are emitted separately by cortex-m-rt and
/// don't belong in `__INTERRUPTS`).
pub fn parse(text: &str) -> Vec<HeaderIrq> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Anchored to the line start (after optional whitespace) so we don't
        // match `#define FOO_IRQn = 12` style macros or in-comment mentions.
        Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)_IRQn\s*=\s*(-?\d+)\s*,?")
            .expect("regex compiles")
    });

    let mut out = Vec::new();
    for line in text.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let name = caps.get(1).unwrap().as_str().to_string();
        let raw = caps.get(2).unwrap().as_str();
        let Ok(value): Result<i64, _> = raw.parse() else { continue };
        if value < 0 {
            // Cortex-M core exceptions (HardFault, MemoryManagement, etc.).
            // Not part of the device-specific vector table.
            continue;
        }
        out.push(HeaderIrq {
            name,
            value: value as u32,
        });
    }
    out
}

/// Convenience wrapper: read a header from disk and parse it.
pub fn parse_file(path: impl AsRef<Path>) -> Result<Vec<HeaderIrq>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read header {}", path.display()))?;
    Ok(parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check against a slice of the EFR32MG26 device header. The
    /// radio peripherals (FRC, MODEM, AGC, BUFC, PROTIMER, SYNTH,
    /// RAC_RSM, RAC_SEQ, RFECA0, RFECA1) live in the public C header but
    /// not the public SVD — those are exactly the IRQs this parser exists
    /// to recover. Includes a negative-valued Cortex-M core exception
    /// (`MemoryManagement_IRQn = -12`) to confirm the parser drops it.
    #[test]
    fn parses_efr32mg26_radio_irqs() {
        let sample = r#"
/* Excerpt from efr32mg26b420f3200im68.h */
typedef enum IRQn {
  Reset_IRQn             = -15,
  NonMaskableInt_IRQn    = -14,
  MemoryManagement_IRQn  = -12,
  SMU_SECURE_IRQn        = 0,  /*!<  0 EFR32 SMU_SECURE Interrupt */
  EMU_IRQn               = 3,
  TIMER0_IRQn            = 4,
  AGC_IRQn               = 46,
  BUFC_IRQn              = 47,
  FRC_PRI_IRQn           = 48,
  FRC_IRQn               = 49,
  MODEM_IRQn             = 50,
  PROTIMER_IRQn          = 51,
  RAC_RSM_IRQn           = 52,
  RAC_SEQ_IRQn           = 53,
  SYNTH_IRQn             = 55,
  RFECA0_IRQn            = 86,
  RFECA1_IRQn            = 87,
} IRQn_Type;
        "#;

        let irqs = parse(sample);
        let by_name: std::collections::HashMap<&str, u32> =
            irqs.iter().map(|i| (i.name.as_str(), i.value)).collect();

        // Radio peripherals — missing from the SVD.
        assert_eq!(by_name.get("FRC"), Some(&49));
        assert_eq!(by_name.get("MODEM"), Some(&50));
        assert_eq!(by_name.get("AGC"), Some(&46));
        assert_eq!(by_name.get("BUFC"), Some(&47));
        assert_eq!(by_name.get("FRC_PRI"), Some(&48));
        assert_eq!(by_name.get("PROTIMER"), Some(&51));
        assert_eq!(by_name.get("RAC_RSM"), Some(&52));
        assert_eq!(by_name.get("RAC_SEQ"), Some(&53));
        assert_eq!(by_name.get("SYNTH"), Some(&55));
        assert_eq!(by_name.get("RFECA0"), Some(&86));
        assert_eq!(by_name.get("RFECA1"), Some(&87));

        // Already in the SVD — must still be picked up.
        assert_eq!(by_name.get("TIMER0"), Some(&4));
        assert_eq!(by_name.get("EMU"), Some(&3));
        assert_eq!(by_name.get("SMU_SECURE"), Some(&0));

        // Cortex-M core exceptions are skipped.
        assert!(!by_name.contains_key("Reset"));
        assert!(!by_name.contains_key("NonMaskableInt"));
        assert!(!by_name.contains_key("MemoryManagement"));
    }

    #[test]
    fn ignores_non_irq_lines() {
        let sample = r#"
#define SOMETHING_IRQn 99
// Comment: this looks like AGC_IRQn = 46 but is a comment
struct foo { int FRC_IRQn; };
        "#;
        // Only a true enum-style line `NAME_IRQn = N,` should match.
        let irqs = parse(sample);
        let names: Vec<&str> = irqs.iter().map(|i| i.name.as_str()).collect();
        assert!(names.is_empty(), "unexpected matches: {names:?}");
    }
}
