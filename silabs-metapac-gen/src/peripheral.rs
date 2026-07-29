use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

/// Strip paired `_S` TrustZone-alias register definitions from an SVD XML.
///
/// Series 2 Silabs SVDs duplicate every peripheral as `<base>_NS` (non-secure)
/// and `<base>_S` (secure) with identical register layouts. Silabs's mapping is
/// the *opposite* of the typical ARMv8-M convention: NS lives at `0x5xxx_xxxx`
/// and S at `0x4xxx_xxxx` for most peripherals. For register codegen we drop
/// the `_S` definition only when its `_NS` peer exists, so chiptool sees one
/// register-block definition per IP. The per-chip instance inventory is built
/// separately from the original SVD and retains both exact base addresses.
///
/// Returns the rewritten XML.
pub fn strip_secure_peripherals(xml: &str) -> Result<String> {
    let peripheral_names = collect_peripheral_names(xml)?;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Writer::new(Cursor::new(Vec::new()));

    // We're inside <peripherals><peripheral>...</peripheral></peripherals>.
    // Walk: find a <peripheral> start tag, capture all events until matching </peripheral>,
    // peek at <name> inside the captured events, and either re-emit or skip the whole block.
    loop {
        let ev = reader.read_event_into(&mut buf)?;
        match &ev {
            Event::Start(e) if e.name().as_ref() == b"peripheral" => {
                // Capture this peripheral.
                let mut block_events: Vec<Event<'static>> = vec![ev.clone().into_owned()];
                let mut name = String::new();
                // `elem_depth` tracks element nesting *inside* this peripheral.
                // Depth 1 means we're a direct child of <peripheral>.
                let mut elem_depth: i32 = 0;
                let mut cap_buf = Vec::new();
                let mut in_name_top = false;
                let mut done = false;
                while !done {
                    let cev = reader.read_event_into(&mut cap_buf)?;
                    match &cev {
                        Event::Start(s) => {
                            elem_depth += 1;
                            if s.name().as_ref() == b"name" && elem_depth == 1 {
                                in_name_top = true;
                            }
                        }
                        Event::End(e) => {
                            if e.name().as_ref() == b"peripheral" && elem_depth == 0 {
                                done = true;
                            } else {
                                elem_depth -= 1;
                            }
                        }
                        Event::Text(t) if in_name_top => {
                            name = t.decode()?.to_string();
                            in_name_top = false;
                        }
                        Event::Empty(_) => {}
                        _ => {}
                    }
                    block_events.push(cev.into_owned());
                    if done {
                        break;
                    }
                    cap_buf.clear();
                }
                // Drop only a confirmed alias pair. A future peripheral with an
                // `_S`-shaped name but no `_NS` peer remains intact.
                let paired_secure_alias =
                    secure_to_nonsecure_name(&name).is_some_and(|peer| peripheral_names.contains(&peer));
                if paired_secure_alias {
                    // Drop the entire block.
                } else {
                    for e in block_events {
                        out.write_event(e)?;
                    }
                }
            }
            Event::Eof => break,
            other => out.write_event(other.clone())?,
        }
        buf.clear();
    }
    let inner = out.into_inner().into_inner();
    Ok(String::from_utf8(inner)?)
}

/// Return the corresponding non-secure alias name for an `_S`/`_S_` name.
pub fn secure_to_nonsecure_name(name: &str) -> Option<String> {
    if let Some(base) = name.strip_suffix("_S") {
        return Some(format!("{base}_NS"));
    }
    name.find("_S_").map(|at| {
        let mut peer = name.to_owned();
        peer.replace_range(at..at + 3, "_NS_");
        peer
    })
}

/// Return the corresponding secure alias name for an `_NS`/`_NS_` name.
pub(crate) fn nonsecure_to_secure_name(name: &str) -> Option<String> {
    if let Some(base) = name.strip_suffix("_NS") {
        return Some(format!("{base}_S"));
    }
    name.find("_NS_").map(|at| {
        let mut peer = name.to_owned();
        peer.replace_range(at..at + 4, "_S_");
        peer
    })
}

fn collect_peripheral_names(xml: &str) -> Result<BTreeSet<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut names = BTreeSet::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) if e.name().as_ref() == b"peripheral" => {
                let mut elem_depth = 0;
                let mut in_name_top = false;
                let mut name = String::new();
                let mut inner_buf = Vec::new();
                loop {
                    match reader.read_event_into(&mut inner_buf)? {
                        Event::Start(e) => {
                            elem_depth += 1;
                            if e.name().as_ref() == b"name" && elem_depth == 1 {
                                in_name_top = true;
                            }
                        }
                        Event::End(e) => {
                            if e.name().as_ref() == b"peripheral" && elem_depth == 0 {
                                break;
                            }
                            elem_depth -= 1;
                        }
                        Event::Text(t) if in_name_top => {
                            name = t.decode()?.to_string();
                            in_name_top = false;
                        }
                        Event::Eof => break,
                        _ => {}
                    }
                    inner_buf.clear();
                }
                names.insert(name);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_secure_peripherals() {
        let xml = include_str!("../../silabs-data-gen/tests/fixtures/eusart0_min.svd");
        let stripped = strip_secure_peripherals(xml).unwrap();
        assert!(stripped.contains("EUSART0_NS"), "NS must remain");
        assert!(
            !stripped.contains("EUSART0_S</name>"),
            "S must be removed (got: {})",
            stripped
        );
    }

    #[test]
    fn alias_name_conversion_handles_suffix_and_infix_forms() {
        assert_eq!(secure_to_nonsecure_name("GPIO_S").as_deref(), Some("GPIO_NS"));
        assert_eq!(nonsecure_to_secure_name("GPIO_NS").as_deref(), Some("GPIO_S"));
        assert_eq!(
            secure_to_nonsecure_name("SEMAILBOX_S_HOST").as_deref(),
            Some("SEMAILBOX_NS_HOST")
        );
        assert_eq!(
            nonsecure_to_secure_name("SEMAILBOX_NS_HOST").as_deref(),
            Some("SEMAILBOX_S_HOST")
        );
        assert_eq!(secure_to_nonsecure_name("GPIO"), None);
    }

    #[test]
    fn keeps_secure_shaped_name_without_nonsecure_peer() {
        let xml = r#"<device><peripherals><peripheral>
            <name>ONLY_S_PORT</name><baseAddress>0x40000000</baseAddress>
        </peripheral></peripherals></device>"#;
        let stripped = strip_secure_peripherals(xml).unwrap();
        assert!(stripped.contains("ONLY_S_PORT"));
    }

    /// Strip-test against an MG26-shaped fixture with multiple NS/S pairs.
    /// Hermetic — no dependency on a vendored pack being extracted.
    /// Addresses copied verbatim from EFR32MG26B211F2048IM68.svd.
    #[test]
    fn drops_secure_in_mg26_shaped_svd() {
        // The fixture lives in the silabs-metapac-gen tests dir.
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mg26_smoke.svd");
        let xml = std::fs::read_to_string(&fixture).unwrap_or_else(|e| panic!("read {}: {e}", fixture.display()));

        // Sanity: original has both NS and S peripherals (2 of each in fixture).
        let orig = peripheral_names(&xml);
        let orig_ns: Vec<_> = orig.iter().filter(|n| n.ends_with("_NS")).collect();
        let orig_s: Vec<_> = orig
            .iter()
            .filter(|n| n.ends_with("_S") && !n.ends_with("_NS"))
            .collect();
        assert_eq!(orig_ns.len(), 2, "original NS count: {orig_ns:?}");
        assert_eq!(orig_s.len(), 2, "original S count: {orig_s:?}");

        let stripped = strip_secure_peripherals(&xml).unwrap();
        let stripped_names = peripheral_names(&stripped);
        let ns: Vec<_> = stripped_names.iter().filter(|n| n.ends_with("_NS")).collect();
        let s: Vec<_> = stripped_names
            .iter()
            .filter(|n| n.ends_with("_S") && !n.ends_with("_NS"))
            .collect();
        assert_eq!(ns.len(), 2, "NS peripheral count after strip: {ns:?}");
        assert_eq!(s.len(), 0, "S peripherals after strip: {s:?}");
    }

    /// Pull only top-level `<peripheral><name>...</name></peripheral>` names.
    fn peripheral_names(xml: &str) -> Vec<String> {
        collect_peripheral_names(xml).unwrap().into_iter().collect()
    }
}
