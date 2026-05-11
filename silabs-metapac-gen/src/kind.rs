//! Canonical IP-kind extraction from SVD peripheral names.
//!
//! Silabs SVD peripherals come in flavours like `EUSART0_NS`, `TIMER10_NS`,
//! `GPIO_NS`, plus the `_S` TrustZone-alias duplicates (already stripped before
//! we get here). The canonical "kind" is the peripheral family with both the
//! TrustZone suffix (`_NS`/`_S`) and any trailing instance digits removed:
//! `EUSART0_NS` → `EUSART`, `TIMER10_NS` → `TIMER`, `GPIO_NS` → `GPIO`.
//!
//! The kind is the key (along with the SVD `<peripheral><version>` field) for
//! the `data/registers/<kind_lc>_v<version>.yaml` artefact and the
//! `src/registers/<kind_lc>_v<version>.rs` Rust module.

use anyhow::{Result, anyhow};

/// A canonical IP kind name in UPPER_SNAKE form (e.g. `EUSART`, `GPIO`).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Kind(String);

impl Kind {
    /// Parse a SVD peripheral name into its canonical kind.
    ///
    /// Strips any `_NS`/`_S` suffix, then any trailing ASCII digits.
    /// Empty result is rejected.
    pub fn from_peripheral_name(name: &str) -> Result<Self> {
        let stripped = name
            .strip_suffix("_NS")
            .or_else(|| name.strip_suffix("_S"))
            .unwrap_or(name);
        let trimmed = stripped.trim_end_matches(|c: char| c.is_ascii_digit());
        let base = if trimmed.is_empty() { stripped } else { trimmed };
        if base.is_empty() {
            return Err(anyhow!(
                "cannot derive kind from peripheral name `{name}`"
            ));
        }
        Ok(Kind(base.to_ascii_uppercase()))
    }

    /// UPPER_SNAKE-case kind name (e.g. `EUSART`).
    pub fn as_upper(&self) -> &str {
        &self.0
    }

    /// Lowercased kind name for filenames and module identifiers
    /// (e.g. `eusart`).
    pub fn lowercase(&self) -> String {
        self.0.to_ascii_lowercase()
    }

    /// PascalCase kind name suitable for use as a Rust struct identifier
    /// (e.g. `Eusart`). The first character is uppercased, the rest are
    /// lowercased — Silabs kinds are short acronyms so we don't try to be
    /// clever about word boundaries.
    pub fn pascal_case(&self) -> String {
        let mut chars = self.0.chars();
        match chars.next() {
            None => String::new(),
            Some(first) => {
                let mut out = String::new();
                out.push(first.to_ascii_uppercase());
                for c in chars {
                    out.push(c.to_ascii_lowercase());
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ns_and_digits() {
        assert_eq!(Kind::from_peripheral_name("EUSART0_NS").unwrap().as_upper(), "EUSART");
        assert_eq!(Kind::from_peripheral_name("TIMER10_NS").unwrap().as_upper(), "TIMER");
        assert_eq!(Kind::from_peripheral_name("GPIO_NS").unwrap().as_upper(), "GPIO");
        assert_eq!(Kind::from_peripheral_name("ACMP1_S").unwrap().as_upper(), "ACMP");
    }

    #[test]
    fn handles_no_suffix() {
        assert_eq!(Kind::from_peripheral_name("DCDC").unwrap().as_upper(), "DCDC");
        assert_eq!(Kind::from_peripheral_name("MSC").unwrap().as_upper(), "MSC");
    }

    #[test]
    fn pascal_case_roundtrip() {
        assert_eq!(Kind::from_peripheral_name("EUSART0_NS").unwrap().pascal_case(), "Eusart");
        assert_eq!(Kind::from_peripheral_name("GPIO_NS").unwrap().pascal_case(), "Gpio");
        assert_eq!(Kind::from_peripheral_name("CMU_NS").unwrap().pascal_case(), "Cmu");
    }

    #[test]
    fn lowercase() {
        assert_eq!(Kind::from_peripheral_name("EUSART0_NS").unwrap().lowercase(), "eusart");
    }
}
