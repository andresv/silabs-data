//! Attach each IRQ of a chip to its peripheral.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One IRQ of a peripheral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeripheralInterrupt {
    /// `GLOBAL` when the IRQ belongs to the whole peripheral, else the IRQ
    /// name suffix (`RX`, `APP`, `ODD`).
    pub signal: String,
    /// IRQ name from the device header.
    pub interrupt: String,
}

/// IRQs whose name matches no peripheral, as `(IRQ, peripheral, signal)`.
/// The device headers give `DMEM` the `MPAHBRAM_TypeDef` type.
const OVERRIDES: &[(&str, &str, &str)] = &[
    ("IADC", "IADC0", "GLOBAL"),
    ("VDAC", "VDAC0", "GLOBAL"),
    ("SYSRTC_APP", "SYSRTC0", "APP"),
    ("SYSRTC_SEQ", "SYSRTC0", "SEQ"),
    ("SEMBRX", "SEMAILBOX_NS_HOST", "RX"),
    ("SEMBTX", "SEMAILBOX_NS_HOST", "TX"),
    ("TRNG", "CRYPTOACC", "TRNG"),
    ("PKE", "CRYPTOACC", "PKE"),
    ("MPAHBRAM", "DMEM", "GLOBAL"),
    ("MPAHBRAM0", "DMEM0", "GLOBAL"),
    ("MPAHBRAM1", "DMEM1", "GLOBAL"),
    ("USB", "USB_NS_APBS", "GLOBAL"),
];

/// IRQs of each canonical peripheral name, in the order of `irqs`.
///
/// An IRQ named like a peripheral gets `GLOBAL`. An IRQ named
/// `<peripheral>_<suffix>` gets `<suffix>`, and the longest peripheral name
/// wins, so `TIMER10_X` never goes to `TIMER1`. [`OVERRIDES`] maps the rest.
/// Radio, software, kernel, CTI and bus-bridge IRQs get no peripheral.
pub fn attach(peripherals: &[&str], irqs: &[&str]) -> BTreeMap<String, Vec<PeripheralInterrupt>> {
    let mut out: BTreeMap<String, Vec<PeripheralInterrupt>> = BTreeMap::new();
    for &irq in irqs {
        let by_name = peripherals
            .iter()
            .filter_map(|&p| {
                if irq == p {
                    Some((p, "GLOBAL"))
                } else {
                    irq.strip_prefix(p)?.strip_prefix('_').map(|signal| (p, signal))
                }
            })
            .max_by_key(|(p, _)| p.len());
        let found = by_name.or_else(|| {
            OVERRIDES
                .iter()
                .find(|(i, p, _)| *i == irq && peripherals.contains(p))
                .map(|&(_, p, signal)| (p, signal))
        });
        if let Some((p, signal)) = found {
            out.entry(p.to_owned()).or_default().push(PeripheralInterrupt {
                signal: signal.to_owned(),
                interrupt: irq.to_owned(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupts_attach_by_name_suffix_and_table() {
        let got = attach(
            &["TIMER1", "TIMER10", "EUSART0", "IADC0"],
            &["TIMER1", "TIMER10", "EUSART0_RX", "EUSART0_TX", "IADC", "FRC"],
        );
        let rows = |p: &str| -> Vec<(&str, &str)> {
            got[p]
                .iter()
                .map(|i| (i.signal.as_str(), i.interrupt.as_str()))
                .collect()
        };
        assert_eq!(rows("TIMER1"), [("GLOBAL", "TIMER1")]);
        assert_eq!(rows("TIMER10"), [("GLOBAL", "TIMER10")]);
        assert_eq!(rows("EUSART0"), [("RX", "EUSART0_RX"), ("TX", "EUSART0_TX")]);
        assert_eq!(rows("IADC0"), [("GLOBAL", "IADC")]);
        assert!(got.values().flatten().all(|i| i.interrupt != "FRC"));
    }
}
