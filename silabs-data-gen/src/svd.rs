use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeripheralIr {
    pub name: String,
    pub base_address: u64,
    pub version: Option<String>,
    pub registers: Vec<RegisterIr>,
    /// sha256 hex of canonical-string of (sorted-by-offset registers).
    /// Excludes name and base_address — so NS/S aliases hash equal.
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterIr {
    pub name: String,
    pub offset: u32,
    pub size: u32, // bits
    pub reset: u64,
}

/// Parse all peripherals from an SVD document.
///
/// SVD `<interrupt>` blocks are intentionally not extracted — the CMSIS
/// device header is the authoritative IRQ source (see
/// `silabs_data_gen::header`). The interrupt table for a chip is built
/// from the header alone, matching stm32-data.
pub fn parse(xml: &str) -> Result<Vec<PeripheralIr>> {
    let raw = parse_raw(xml)?;
    let mut peripherals: Vec<PeripheralIr> = Vec::with_capacity(raw.peripherals.len());

    // Build a map of name -> raw peripheral for derivedFrom resolution.
    let by_name: HashMap<String, &RawPeripheral> = raw.peripherals.iter().map(|p| (p.name.clone(), p)).collect();

    let dev_size = raw.device_size.unwrap_or(32);
    let dev_reset = raw.device_reset.unwrap_or(0);

    for p in &raw.peripherals {
        // If this peripheral derives from another, copy that one's registers.
        let registers_src = if let Some(parent_name) = &p.derived_from {
            let parent = by_name
                .get(parent_name.as_str())
                .ok_or_else(|| anyhow!("peripheral {} derivedFrom {} not found", p.name, parent_name))?;
            &parent.registers
        } else {
            &p.registers
        };

        let p_size = p.size.or_else(|| {
            p.derived_from
                .as_ref()
                .and_then(|n| by_name.get(n.as_str()).and_then(|x| x.size))
        });
        let p_reset = p.reset.or_else(|| {
            p.derived_from
                .as_ref()
                .and_then(|n| by_name.get(n.as_str()).and_then(|x| x.reset))
        });

        let mut regs: Vec<RegisterIr> = registers_src
            .iter()
            .map(|r| RegisterIr {
                name: r.name.clone(),
                offset: r.offset,
                size: r.size.or(p_size).unwrap_or(dev_size),
                reset: r.reset.or(p_reset).unwrap_or(dev_reset),
            })
            .collect();
        regs.sort_by_key(|r| r.offset);

        let fingerprint = compute_fingerprint(&regs);

        peripherals.push(PeripheralIr {
            name: p.name.clone(),
            base_address: p.base_address,
            version: p.version.clone(),
            registers: regs,
            fingerprint,
        });
    }

    Ok(peripherals)
}

fn compute_fingerprint(regs: &[RegisterIr]) -> String {
    // Canonical: registers sorted by offset, fields joined with '\n'.
    // Field order per register: offset_hex, size_dec, reset_hex, name.
    let mut buf = String::with_capacity(regs.len() * 32);
    for r in regs {
        use std::fmt::Write as _;
        let _ = writeln!(&mut buf, "{:08x}\t{}\t{:016x}\t{}", r.offset, r.size, r.reset, r.name);
    }
    let digest = Sha256::digest(buf.as_bytes());
    hex(&digest)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

// --- Raw parse types --------------------------------------------------------

#[derive(Debug, Default)]
struct RawDevice {
    device_size: Option<u32>,
    device_reset: Option<u64>,
    peripherals: Vec<RawPeripheral>,
}

#[derive(Debug, Default)]
struct RawPeripheral {
    name: String,
    derived_from: Option<String>,
    base_address: u64,
    version: Option<String>,
    size: Option<u32>,
    reset: Option<u64>,
    registers: Vec<RawRegister>,
}

#[derive(Debug, Default)]
struct RawRegister {
    name: String,
    offset: u32,
    size: Option<u32>,
    reset: Option<u64>,
}

fn parse_raw(xml: &str) -> Result<RawDevice> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();

    let mut dev = RawDevice::default();
    let mut cur_periph: Option<RawPeripheral> = None;
    let mut cur_reg: Option<RawRegister> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => {
                let local = std::str::from_utf8(e.name().as_ref())?.to_owned();
                match local.as_str() {
                    "peripheral" => {
                        let mut p = RawPeripheral::default();
                        for a in e.attributes() {
                            let a = a?;
                            if std::str::from_utf8(a.key.as_ref())? == "derivedFrom" {
                                p.derived_from = Some(a.unescape_value()?.into_owned());
                            }
                        }
                        cur_periph = Some(p);
                    }
                    "register" => {
                        cur_reg = Some(RawRegister::default());
                    }
                    _ => {}
                }
                path.push(local);
            }
            Event::Empty(e) => {
                let local = std::str::from_utf8(e.name().as_ref())?.to_owned();
                if local == "peripheral" {
                    let mut p = RawPeripheral::default();
                    for a in e.attributes() {
                        let a = a?;
                        if std::str::from_utf8(a.key.as_ref())? == "derivedFrom" {
                            p.derived_from = Some(a.unescape_value()?.into_owned());
                        }
                    }
                    // Empty peripheral element — finalize immediately if we
                    // somehow get one. Only meaningful when derivedFrom is set
                    // and the peripheral inherits all properties.
                    dev.peripherals.push(p);
                }
            }
            Event::Text(t) => {
                let txt = t.decode()?.into_owned();
                let trimmed = txt.trim();
                if trimmed.is_empty() {
                    buf.clear();
                    continue;
                }
                let depth = path.len();
                let last = path.last().map(String::as_str).unwrap_or("");
                let parent = path.get(depth.wrapping_sub(2)).map(String::as_str).unwrap_or("");
                let grand = path.get(depth.wrapping_sub(3)).map(String::as_str).unwrap_or("");

                // Device-level defaults (depth: device > size/resetValue).
                if depth == 2 && parent == "device" {
                    match last {
                        "size" => {
                            dev.device_size = Some(parse_uint::<u32>(trimmed)?);
                        }
                        "resetValue" => {
                            dev.device_reset = Some(parse_uint::<u64>(trimmed)?);
                        }
                        _ => {}
                    }
                }

                // Field collection within a register.
                if let Some(reg) = cur_reg.as_mut() {
                    // We are inside a <register> when last comes from register children.
                    // The register children we care about: name, addressOffset, size, resetValue.
                    if grand == "register" || parent == "register" {
                        match last {
                            "name" => reg.name = trimmed.to_string(),
                            "addressOffset" => {
                                reg.offset = parse_uint::<u32>(trimmed)?;
                            }
                            "size" => {
                                reg.size = Some(parse_uint::<u32>(trimmed)?);
                            }
                            "resetValue" => {
                                reg.reset = Some(parse_uint::<u64>(trimmed)?);
                            }
                            _ => {}
                        }
                    }
                }

                // Peripheral-level direct fields (only when not inside register).
                // The SVD's `<interrupt>` blocks are ignored — see `parse` doc.
                if let Some(p) = cur_periph.as_mut()
                    && cur_reg.is_none()
                    && parent == "peripheral"
                {
                    match last {
                        "name" => p.name = trimmed.to_string(),
                        "baseAddress" => {
                            p.base_address = parse_uint::<u64>(trimmed)?;
                        }
                        "version" => p.version = Some(trimmed.to_string()),
                        "size" => p.size = Some(parse_uint::<u32>(trimmed)?),
                        "resetValue" => p.reset = Some(parse_uint::<u64>(trimmed)?),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let local = std::str::from_utf8(e.name().as_ref())?.to_owned();
                match local.as_str() {
                    "peripheral" => {
                        if let Some(p) = cur_periph.take() {
                            dev.peripherals.push(p);
                        }
                    }
                    "register" => {
                        if let Some(r) = cur_reg.take()
                            && let Some(p) = cur_periph.as_mut()
                        {
                            p.registers.push(r);
                        }
                    }
                    _ => {}
                }
                path.pop();
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(dev)
}

fn parse_uint<T>(s: &str) -> Result<T>
where
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: std::fmt::Debug,
{
    let v = parse_uint_u64(s)?;
    T::try_from(v).map_err(|e| anyhow!("value {} out of range: {:?}", s, e))
}

fn parse_uint_u64(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    if let Some(stripped) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(stripped, 16).with_context(|| format!("invalid hex literal {trimmed:?}"))
    } else if trimmed.is_empty() {
        bail!("empty numeric literal");
    } else if let Some(rest) = trimmed.strip_prefix('#') {
        // Some SVDs use "#1" for binary or "#0" for decimal.
        rest.parse::<u64>()
            .with_context(|| format!("invalid binary/dec literal {trimmed:?}"))
    } else {
        trimmed
            .parse::<u64>()
            .with_context(|| format!("invalid decimal literal {trimmed:?}"))
    }
}
