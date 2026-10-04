//! Facts from a Silicon Labs CMSIS device header: the IRQn enum, series and
//! config, NVIC priority bits, flash page size, bonded GPIO pins, DMA channel
//! count and DMA requests.
//!
//! The SVDs omit the `<interrupt>` blocks of the radio peripherals (FRC, MODEM,
//! AGC, BUFC, PROTIMER, SYNTH, RAC_RSM, RAC_SEQ, RFECA0, RFECA1, …). The
//! per-chip header `Device/SiliconLabs/<FAMILY>/Include/<chip>.h` has the full
//! IRQ enum:
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
//! `chips::build` uses these entries as the interrupt table and ignores the
//! SVD `<interrupt>` blocks.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;

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
/// Skips negative values. They are Cortex-M core exceptions
/// (`HardFault_IRQn = -13`), which cortex-m-rt emits itself, so they do not
/// belong in `__INTERRUPTS`.
pub fn parse(text: &str) -> Vec<HeaderIrq> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Anchored to the line start, so `#define FOO_IRQn = 12` macros and
        // mentions in comments do not match.
        Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)_IRQn\s*=\s*(-?\d+)\s*,?").expect("regex compiles")
    });

    let mut out = Vec::new();
    for line in text.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let name = caps.get(1).unwrap().as_str().to_string();
        let raw = caps.get(2).unwrap().as_str();
        let Ok(value): Result<i64, _> = raw.parse() else {
            continue;
        };
        if value < 0 {
            continue;
        }
        out.push(HeaderIrq {
            name,
            value: value as u32,
        });
    }
    out
}

pub fn parse_file(path: impl AsRef<Path>) -> Result<Vec<HeaderIrq>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).with_context(|| format!("read header {}", path.display()))?;
    Ok(parse(&text))
}

/// Silicon Labs chip generation + within-series config, extracted from
/// the per-chip CMSIS device header's `_SILICON_LABS_32B_SERIES` and
/// `_SILICON_LABS_32B_SERIES_<N>_CONFIG` macros.
#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Series {
    /// Series number (`_SILICON_LABS_32B_SERIES`). `0` for EFM32 Gecko
    /// (Cortex-M0+/M3), `1` for EFM32/EFR32 xG1x (Cortex-M4), `2` for
    /// Cortex-M33 Series 2 chips (xG21..xG29), `3` for newer `SI`-prefixed
    /// Series 3 chips.
    pub series: u8,
    /// Within-series config number (`_SILICON_LABS_32B_SERIES_<N>_CONFIG`).
    /// Series 0: always 0 (the header has no config macro). Series 1: 1..4,
    /// not unique per family. Series 2: 1..9 (one per family). Series 3: 301+.
    pub config: u16,
}

/// Parse the series + within-series config from a CMSIS device header.
/// Looks for two `#define` lines:
/// ```c
/// #define _SILICON_LABS_32B_SERIES           2
/// #define _SILICON_LABS_32B_SERIES_2_CONFIG  6
/// ```
/// The valueless companion macros (`_SILICON_LABS_32B_SERIES_2`,
/// `_SILICON_LABS_32B_SERIES_2_CONFIG_6`) are redundant and ignored.
pub fn extract_series(text: &str) -> Result<Series> {
    static SERIES_RE: OnceLock<Regex> = OnceLock::new();
    static CONFIG_RE: OnceLock<Regex> = OnceLock::new();

    let series_re = SERIES_RE.get_or_init(|| {
        // `#define _SILICON_LABS_32B_SERIES <number>` — trailing word
        // boundary prevents matching `_SILICON_LABS_32B_SERIES_2`
        // (no value) or `_SILICON_LABS_32B_SERIES_2_CONFIG`.
        Regex::new(r"^\s*#\s*define\s+_SILICON_LABS_32B_SERIES\s+(\d+)\b").expect("series regex compiles")
    });
    let config_re = CONFIG_RE.get_or_init(|| {
        // `#define _SILICON_LABS_32B_SERIES_<N>_CONFIG <number>`. The `_<N>_`
        // distinguishes from the valueless tag `_SILICON_LABS_32B_SERIES_<N>_CONFIG_<M>`.
        Regex::new(r"^\s*#\s*define\s+_SILICON_LABS_32B_SERIES_\d+_CONFIG\s+(\d+)\b").expect("config regex compiles")
    });

    let mut series: Option<u8> = None;
    let mut config: Option<u16> = None;
    for line in text.lines() {
        if let Some(caps) = series_re.captures(line)
            && let Ok(n) = caps.get(1).unwrap().as_str().parse()
        {
            series = Some(n);
        }
        if let Some(caps) = config_re.captures(line)
            && let Ok(n) = caps.get(1).unwrap().as_str().parse()
        {
            config = Some(n);
        }
    }

    match (series, config) {
        (Some(series), Some(config)) => Ok(Series { series, config }),
        // Series 0 headers define `_SILICON_LABS_32B_SERIES 0` but no
        // `_SILICON_LABS_32B_SERIES_0_CONFIG`. Series 0 has no config axis.
        (Some(0), None) => Ok(Series { series: 0, config: 0 }),
        (None, _) => anyhow::bail!("no `#define _SILICON_LABS_32B_SERIES <N>` found in header"),
        (_, None) => anyhow::bail!("no `#define _SILICON_LABS_32B_SERIES_<N>_CONFIG <M>` found in header"),
    }
}

pub fn extract_series_file(path: impl AsRef<Path>) -> Result<Series> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).with_context(|| format!("read header {}", path.display()))?;
    extract_series(&text)
}

/// Parse `#define __NVIC_PRIO_BITS <N>U` from a CMSIS device header.
/// Seen values: 2 (Cortex-M0+: EFM32ZG/HG), 3 (Cortex-M3/M4: EFM32 Series
/// 0/1), 4 (Cortex-M33: EFR32 Series 2).
pub fn extract_nvic_prio_bits(text: &str) -> Result<u8> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s*#\s*define\s+__NVIC_PRIO_BITS\s+(\d+)U?\b").expect("nvic prio bits regex compiles")
    });
    for line in text.lines() {
        if let Some(caps) = re.captures(line) {
            return caps[1]
                .parse()
                .with_context(|| format!("parse __NVIC_PRIO_BITS in {line:?}"));
        }
    }
    anyhow::bail!("no `#define __NVIC_PRIO_BITS <N>` found in header")
}

pub fn extract_nvic_prio_bits_file(path: impl AsRef<Path>) -> Result<u8> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).with_context(|| format!("read header {}", path.display()))?;
    extract_nvic_prio_bits(&text)
}

/// Parse `#define FLASH_PAGE_SIZE <N>` from a CMSIS device header. Series 2
/// writes `(0x00002000UL)`, Series 0 writes `4096U`.
pub fn parse_flash_page_size(text: &str) -> Option<u32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s*#\s*define\s+FLASH_PAGE_SIZE\s+\(?\s*(0[xX][0-9A-Fa-f]+|\d+)[uUlL]*\s*\)?")
            .expect("flash page size regex compiles")
    });
    text.lines().find_map(|line| {
        let n = &re.captures(line)?[1];
        match n.strip_prefix("0x").or_else(|| n.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => n.parse().ok(),
        }
    })
}

/// Parse the `GPIO_Px_INDEX` / `GPIO_Px_MASK` pairs of a Series 2 device
/// header into `(port index, bonded pin mask)`, sorted by port.
pub fn parse_gpio_port_masks(text: &str) -> Vec<(u8, u32)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s*#\s*define\s+GPIO_P([A-Z])_(INDEX|MASK)\s+\(?\s*(0[xX][0-9A-Fa-f]+|\d+)[uUlL]*\s*\)?")
            .expect("gpio port regex compiles")
    });
    let mut index: BTreeMap<char, u8> = BTreeMap::new();
    let mut mask: BTreeMap<char, u32> = BTreeMap::new();
    for line in text.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let port = caps[1].chars().next().unwrap();
        let Some(n) = parse_c_int(&caps[3]) else { continue };
        if &caps[2] == "INDEX" {
            index.insert(port, n as u8);
        } else {
            mask.insert(port, n as u32);
        }
    }
    let mut out: Vec<(u8, u32)> = mask
        .into_iter()
        .filter_map(|(port, m)| Some((*index.get(&port)?, m)))
        .collect();
    out.sort();
    out
}

/// Number of DMA channels: `LDMA_CH_NUM` (Series 2), else a numeric
/// `DMA_CHAN_COUNT` (Series 0), else 0.
pub fn parse_dma_channel_count(text: &str) -> u8 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s*#\s*define\s+(LDMA_CH_NUM|DMA_CHAN_COUNT)\s+(0[xX][0-9A-Fa-f]+|\d+)[uUlL]*\b")
            .expect("dma channel count regex compiles")
    });
    let mut ldma = None;
    let mut dma = None;
    for line in text.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let n = parse_c_int(&caps[2]).map(|n| n as u8);
        match &caps[1] {
            "LDMA_CH_NUM" => ldma = ldma.or(n),
            _ => dma = dma.or(n),
        }
    }
    ldma.or(dma).unwrap_or(0)
}

/// One DMA request signal from a `*_ldmaxbar_defines.h` or `*_dmareq.h` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderDmaRequest {
    /// Request source, as the header names it (`EUSART0`, `TIMER1`).
    pub source: String,
    /// Signal name without the source prefix (`RXFL`, `CC0`).
    pub signal: String,
    pub sourcesel: u8,
    pub sigsel: u8,
}

/// Parse the Series 2 LDMAXBAR `CH_REQSEL` defines.
///
/// A SIGSEL name joins source and signal with no separator
/// (`_LDMAXBAR_CH_REQSEL_SIGSEL_EUSART0RXFL`), so each one belongs to the
/// longest source name that prefixes it. `NONE` and `PRS` are not request
/// sources.
pub fn parse_ldmaxbar_requests(text: &str) -> Vec<HeaderDmaRequest> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s*#\s*define\s+_LDMAXBAR_CH_REQSEL_(SOURCESEL|SIGSEL)_([A-Za-z0-9_]+)\s+(0[xX][0-9A-Fa-f]+|\d+)[uUlL]*\b")
            .expect("ldmaxbar regex compiles")
    });
    let mut sources: Vec<(String, u8)> = Vec::new();
    let mut signals: Vec<(String, u8)> = Vec::new();
    for line in text.lines() {
        let Some(caps) = re.captures(line) else { continue };
        let Some(n) = parse_c_int(&caps[3]) else { continue };
        let name = caps[2].to_string();
        match &caps[1] {
            "SOURCESEL" if name != "NONE" && name != "PRS" => sources.push((name, n as u8)),
            "SOURCESEL" => {}
            _ => signals.push((name, n as u8)),
        }
    }
    signals
        .into_iter()
        .filter_map(|(name, sigsel)| {
            let Some((source, sourcesel)) = sources
                .iter()
                .filter(|(s, _)| name.starts_with(s.as_str()))
                .max_by_key(|(s, _)| s.len())
            else {
                eprintln!("LDMAXBAR SIGSEL with no SOURCESEL: {name}");
                return None;
            };
            // A signal named like its source (MG26 `SIGSEL_LCD`) is the
            // peripheral's only request, like a `GLOBAL` interrupt.
            let signal = match &name[source.len()..] {
                "" => "GLOBAL",
                rest => rest,
            };
            Some(HeaderDmaRequest {
                source: source.clone(),
                signal: signal.to_string(),
                sourcesel: *sourcesel,
                sigsel,
            })
        })
        .collect()
}

/// Parse the Series 0 `DMAREQ_<SOURCE>_<SIGNAL> ((<sourcesel> << 16) + <sigsel>)`
/// defines. Series 0 source names have no `_`, so the first `_` splits
/// source and signal.
pub fn parse_dmareq_requests(text: &str) -> Vec<HeaderDmaRequest> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^\s*#\s*define\s+DMAREQ_([A-Za-z0-9]+)_([A-Za-z0-9_]+)\s+\(\(\s*(\d+)\s*<<\s*16\s*\)\s*\+\s*(\d+)\s*\)",
        )
        .expect("dmareq regex compiles")
    });
    text.lines()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            Some(HeaderDmaRequest {
                source: caps[1].to_string(),
                signal: caps[2].to_string(),
                sourcesel: caps[3].parse().ok()?,
                sigsel: caps[4].parse().ok()?,
            })
        })
        .collect()
}

/// Parse a C integer literal body (`0x3FF`, `12`) without its suffix.
fn parse_c_int(n: &str) -> Option<u64> {
    match n.strip_prefix("0x").or_else(|| n.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => n.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice of the EFR32MG26 device header. The radio IRQs (FRC, MODEM,
    /// AGC, …) are in the header but not in the SVD.
    /// `MemoryManagement_IRQn = -12` checks that core exceptions are dropped.
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
        let by_name: std::collections::HashMap<&str, u32> = irqs.iter().map(|i| (i.name.as_str(), i.value)).collect();

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

    /// EFR32MG26 header slice with the four `_SILICON_LABS_32B_SERIES*`
    /// macros. The valueless tags (`_SERIES_2`, `_SERIES_2_CONFIG_6`) must
    /// not change the result.
    #[test]
    fn extracts_series_from_efr32mg26_header() {
        let sample = r#"
#define _SILICON_LABS_32B_SERIES_2                                                             /** Product Series Identifier */
#define _SILICON_LABS_32B_SERIES                          2                                    /** Product Series Identifier */
#define _SILICON_LABS_32B_SERIES_2_CONFIG_6                                                    /** Product Config Identifier */
#define _SILICON_LABS_32B_SERIES_2_CONFIG                 6                                    /** Product Config Identifier */
        "#;
        let s = extract_series(sample).expect("extract");
        assert_eq!(s, Series { series: 2, config: 6 });
    }

    /// SIMG301 — Series 3 with 3-digit config number.
    #[test]
    fn extracts_series_3_config_301() {
        let sample = r#"
#define _SILICON_LABS_32B_SERIES                          3                              /** Product Series Identifier */
#define _SILICON_LABS_32B_SERIES_3                                                       /** Product Series Identifier */
#define _SILICON_LABS_32B_SERIES_3_CONFIG                 301                            /** Product Config Identifier */
#define _SILICON_LABS_32B_SERIES_3_CONFIG_301                                            /** Product Config Identifier */
        "#;
        let s = extract_series(sample).expect("extract");
        assert_eq!(s, Series { series: 3, config: 301 });
    }

    /// EFM32GG390F1024 — Series 0 headers carry `_SILICON_LABS_32B_SERIES 0`
    /// but no `_SILICON_LABS_32B_SERIES_0_CONFIG` macro.
    #[test]
    fn extracts_series_0_without_config() {
        let sample = r#"
#define _SILICON_LABS_32B_SERIES_0                 /**< Silicon Labs series number */
#define _SILICON_LABS_32B_SERIES                0  /**< Silicon Labs series number */
#define _SILICON_LABS_GECKO_INTERNAL_SDID       72 /**< Silicon Labs internal use only, may change any time */
        "#;
        let s = extract_series(sample).expect("extract");
        assert_eq!(s, Series { series: 0, config: 0 });
    }

    /// EFM32GG11B820F2048GL192 — Series 1 has the same macro shape as Series 2.
    #[test]
    fn extracts_series_1_config() {
        let sample = r#"
#define _SILICON_LABS_32B_SERIES_1                   /**< Silicon Labs series number */
#define _SILICON_LABS_32B_SERIES                 1   /**< Silicon Labs series number */
#define _SILICON_LABS_32B_SERIES_1_CONFIG_1          /**< Series 1, Configuration 1 */
#define _SILICON_LABS_32B_SERIES_1_CONFIG        1   /**< Series 1, Configuration 1 */
        "#;
        let s = extract_series(sample).expect("extract");
        assert_eq!(s, Series { series: 1, config: 1 });
    }

    /// The Series 0 exemption must not hide a missing config on Series 2.
    #[test]
    fn extract_series_still_requires_config_for_series_2() {
        let err = extract_series("#define _SILICON_LABS_32B_SERIES 2\n").unwrap_err();
        assert!(err.to_string().contains("_CONFIG"), "{err}");
    }

    #[test]
    fn extracts_nvic_prio_bits() {
        // EFM32ZG222F32 (Cortex-M0+).
        let m0 = "#define __NVIC_PRIO_BITS          2U /**< NVIC interrupt priority bits */\n";
        assert_eq!(extract_nvic_prio_bits(m0).unwrap(), 2);
        // EFM32GG390F1024 (Cortex-M3).
        let m3 = "#define __NVIC_PRIO_BITS          3U /**< NVIC interrupt priority bits */\n";
        assert_eq!(extract_nvic_prio_bits(m3).unwrap(), 3);
        // EFR32MG24B210F1536IM48 (Cortex-M33).
        let m33 = "#define __NVIC_PRIO_BITS          4U      /**< NVIC interrupt priority bits */\n";
        assert_eq!(extract_nvic_prio_bits(m33).unwrap(), 4);
    }

    #[test]
    fn extract_nvic_prio_bits_rejects_missing_macro() {
        let err = extract_nvic_prio_bits("nothing here").unwrap_err();
        assert!(err.to_string().contains("__NVIC_PRIO_BITS"), "{err}");
    }

    #[test]
    fn parses_flash_page_size() {
        let sample = "#define ICACHE0_FLASH_SIZE                      0x180000UL  /**> Flash size */\n\
                      #define FLASH_PAGE_SIZE                                   (0x00002000UL) /**< Flash Memory page size */\n";
        assert_eq!(parse_flash_page_size(sample), Some(0x2000));
        assert_eq!(
            parse_flash_page_size(
                "#define FLASH_PAGE_SIZE         4096U                  /**< Flash Memory page size */\n"
            ),
            Some(4096)
        );
        assert_eq!(
            parse_flash_page_size("#define ICACHE0_FLASH_SIZE                      0x180000UL  /**> Flash size */\n"),
            None
        );
    }

    #[test]
    fn parses_gpio_port_masks() {
        let sample = "#define GPIO_PA_INDEX                                     0U         /**< Index of port PA */\n\
                      #define GPIO_PA_COUNT                                     10U        /**< Number of pins on port PA */\n\
                      #define GPIO_PA_MASK                                      (0x03FFUL) /**< Port PA pin mask */\n\
                      #define GPIO_PB_INDEX                                     1U         /**< Index of port PB */\n\
                      #define GPIO_PB_COUNT                                     6U         /**< Number of pins on port PB */\n\
                      #define GPIO_PB_MASK                                      (0x003FUL) /**< Port PB pin mask */\n";
        assert_eq!(parse_gpio_port_masks(sample), [(0, 0x3FF), (1, 0x3F)]);
    }

    /// TIMER10 is not in a shipped header. It checks that `TIMER10CC0` does
    /// not go to TIMER1.
    #[test]
    fn parses_ldmaxbar_requests() {
        let sample = r#"
#define _LDMAXBAR_CH_REQSEL_SOURCESEL_NONE               0x00000000UL /**< Mode NONE for LDMAXBAR_CH_REQSEL */
#define _LDMAXBAR_CH_REQSEL_SOURCESEL_TIMER1             0x00000003UL /**< Mode TIMER1 for LDMAXBAR_CH_REQSEL */
#define _LDMAXBAR_CH_REQSEL_SOURCESEL_EUSART0            0x0000000fUL /**< Mode EUSART0 for LDMAXBAR_CH_REQSEL */
#define _LDMAXBAR_CH_REQSEL_SOURCESEL_TIMER10            0x00000020UL /**< Mode TIMER10 for LDMAXBAR_CH_REQSEL */
#define LDMAXBAR_CH_REQSEL_SOURCESEL_EUSART0             (_LDMAXBAR_CH_REQSEL_SOURCESEL_EUSART0 << 16)  /**< Shifted Mode EUSART0 for LDMAXBAR_CH_REQSEL */
#define _LDMAXBAR_CH_REQSEL_SIGSEL_TIMER1CC0             0x00000000UL /** Mode TIMER1CC0 for LDMAXBAR_CH_REQSEL**/
#define _LDMAXBAR_CH_REQSEL_SIGSEL_TIMER1UFOF            0x00000003UL /** Mode TIMER1UFOF for LDMAXBAR_CH_REQSEL**/
#define _LDMAXBAR_CH_REQSEL_SIGSEL_EUSART0RXFL           0x00000000UL /** Mode EUSART0RXFL for LDMAXBAR_CH_REQSEL**/
#define _LDMAXBAR_CH_REQSEL_SIGSEL_EUSART0TXFL           0x00000001UL /** Mode EUSART0TXFL for LDMAXBAR_CH_REQSEL**/
#define _LDMAXBAR_CH_REQSEL_SIGSEL_TIMER10CC0            0x00000000UL /** Mode TIMER10CC0 for LDMAXBAR_CH_REQSEL**/
#define _LDMAXBAR_CH_REQSEL_SOURCESEL_LCD                0x0000001eUL /**< Mode LCD for LDMAXBAR_CH_REQSEL */
#define _LDMAXBAR_CH_REQSEL_SIGSEL_LCD                   0x00000000UL /** Mode LCD for LDMAXBAR_CH_REQSEL**/
#define LDMAXBAR_CH_REQSEL_SIGSEL_EUSART0RXFL            (_LDMAXBAR_CH_REQSEL_SIGSEL_EUSART0RXFL << 0)        /** Shifted Mode EUSART0RXFL for LDMAXBAR_CH_REQSEL**/
"#;
        let req = |source: &str, signal: &str, sourcesel, sigsel| HeaderDmaRequest {
            source: source.into(),
            signal: signal.into(),
            sourcesel,
            sigsel,
        };
        assert_eq!(
            parse_ldmaxbar_requests(sample),
            [
                req("TIMER1", "CC0", 3, 0),
                req("TIMER1", "UFOF", 3, 3),
                req("EUSART0", "RXFL", 15, 0),
                req("EUSART0", "TXFL", 15, 1),
                req("TIMER10", "CC0", 32, 0),
                req("LCD", "GLOBAL", 30, 0),
            ]
        );
    }

    #[test]
    fn parses_dmareq_requests() {
        let sample = r#"
#define DMAREQ_USART0_RXDATAV         ((12 << 16) + 0) /**< DMA channel select for USART0_RXDATAV */
#define DMAREQ_USART1_RXDATAVRIGHT    ((13 << 16) + 3) /**< DMA channel select for USART1_RXDATAVRIGHT */
#define DMAREQ_TIMER0_CC2             ((24 << 16) + 3) /**< DMA channel select for TIMER0_CC2 */
"#;
        let req = |source: &str, signal: &str, sourcesel, sigsel| HeaderDmaRequest {
            source: source.into(),
            signal: signal.into(),
            sourcesel,
            sigsel,
        };
        assert_eq!(
            parse_dmareq_requests(sample),
            [
                req("USART0", "RXDATAV", 12, 0),
                req("USART1", "RXDATAVRIGHT", 13, 3),
                req("TIMER0", "CC2", 24, 3),
            ]
        );
    }

    #[test]
    fn extract_series_rejects_header_missing_both_macros() {
        let err = extract_series("nothing relevant in this header").unwrap_err();
        assert!(err.to_string().contains("_SILICON_LABS_32B_SERIES"));
    }
}
