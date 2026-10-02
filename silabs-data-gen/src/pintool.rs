//! One-shot extractor: Silicon Labs pin-tool XML to `data/pins/<family>.yaml`.
//!
//! The pin-tool files are under the MSLA. This module reads them and writes
//! only facts: peripheral, signal, port set, pin name, location number and
//! bus name. Nothing from the XML text goes into the output. After the
//! extraction, the YAML is the only source, and we maintain it by hand.
//! `gen` never calls this module.
//!
//! Each part directory has three files:
//! - `PORTIO.portio`: `<module>` (peripheral) → `<selector>` → `<route>`
//!   (signal) → `<location number portBankIndex pinIndex>`.
//! - `<OPN>.device`: the bonded pins, in `<portBank index>` → `<pin name index>`.
//! - `<OPN>.deviceextension`: per-pin `capability.em2` flags.
//!
//! The extractor builds one table for each family and checks it. For each
//! part, the table filtered by the part's bonded pins must equal the part's
//! own data. If not, the extractor fails and prints the difference.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use quick_xml::events::Event;
use quick_xml::events::attributes::Attributes;
use quick_xml::reader::Reader;

use crate::pins::{Em2, Family, Package, PinId, Signal, pin_name};

/// One signal of one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSignal {
    /// The selector has no `locationPropertyId`: the pins are fixed.
    pub fixed: bool,
    /// Non-empty `aportName` of the route.
    pub bus: Option<String>,
    /// `(location number, pin)` pairs on bonded GPIO pins.
    pub locations: BTreeSet<(u16, PinId)>,
}

/// The pin-tool data of one part.
#[derive(Debug, Clone, Default)]
pub struct Part {
    /// Lowercase part name.
    pub name: String,
    /// Bonded GPIO pins.
    pub bonded: BTreeSet<PinId>,
    /// EM2-capable GPIO pins.
    pub em2: BTreeSet<PinId>,
    /// Peripherals that have a `<module>` on this part.
    pub peripherals: BTreeSet<String>,
    /// `(peripheral, signal)` to the signal data.
    pub signals: BTreeMap<(String, String), PartSignal>,
    /// Locations on pads that are not GPIO pins (for example the dedicated
    /// `AIN0` analog pads), as `PERIPHERAL SIGNAL PAD`. They are not stored.
    pub pads: BTreeSet<String>,
}

fn attr(attrs: Attributes, key: &str) -> Result<Option<String>> {
    for a in attrs {
        let a = a?;
        if a.key.as_ref() == key.as_bytes() {
            return Ok(Some(a.unescape_value()?.into_owned()));
        }
    }
    Ok(None)
}

fn req(attrs: Attributes, key: &str, element: &str) -> Result<String> {
    attr(attrs, key)?.ok_or_else(|| anyhow!("<{element}> has no `{key}`"))
}

/// A name of the form `P<letter><digits>` is a GPIO pin name.
fn looks_like_gpio(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 3 && b[0] == b'P' && b[1].is_ascii_uppercase() && b[2..].iter().all(u8::is_ascii_digit)
}

/// Bonded GPIO pins from `<OPN>.device`, and the port banks that hold
/// non-GPIO pads (bank index to pad name).
fn parse_device(xml: &str, series0: bool) -> Result<(BTreeSet<PinId>, BTreeMap<u8, String>)> {
    let mut reader = Reader::from_str(xml);
    let mut bank: Option<u8> = None;
    let mut bonded = BTreeSet::new();
    let mut pads = BTreeMap::new();
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == b"portBank" => {
                bank = Some(req(e.attributes(), "index", "portBank")?.parse()?);
            }
            Event::End(e) if e.name().as_ref() == b"portBank" => bank = None,
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"pin" => {
                let Some(bank) = bank else { continue };
                let name = req(e.attributes(), "name", "pin")?;
                let index: u8 = req(e.attributes(), "index", "pin")?.parse()?;
                if looks_like_gpio(&name) {
                    ensure!(
                        pin_name((bank, index), series0) == name,
                        "pin `{name}` is in port bank {bank} at index {index}"
                    );
                    bonded.insert((bank, index));
                } else {
                    pads.insert(bank, name);
                }
            }
            _ => {}
        }
    }
    for b in pads.keys() {
        ensure!(
            !bonded.iter().any(|p| p.0 == *b),
            "port bank {b} has both GPIO pins and pads"
        );
    }
    Ok((bonded, pads))
}

/// EM2-capable pins from `<OPN>.deviceextension`.
fn parse_extension(xml: &str, series0: bool) -> Result<BTreeSet<PinId>> {
    let mut reader = Reader::from_str(xml);
    let mut component: Option<String> = None;
    let mut out = BTreeSet::new();
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == b"componentExtension" => {
                component = attr(e.attributes(), "componentName")?;
            }
            Event::End(e) if e.name().as_ref() == b"componentExtension" => component = None,
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"boolProperty" => {
                if attr(e.attributes(), "id")?.as_deref() != Some("capability.em2") {
                    continue;
                }
                let Some(c) = &component else { continue };
                let value = req(e.attributes(), "defaultValue", "boolProperty")?;
                ensure!(value == "true" || value == "false", "{c}: capability.em2 = {value}");
                if value == "true" {
                    ensure!(looks_like_gpio(c), "capability.em2 on `{c}`, which is not a pin");
                    out.insert(crate::pins::parse_pin(c, series0)?);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Parse the three files of one part.
pub fn parse_part(name: &str, series0: bool, device: &str, extension: &str, portio: &str) -> Result<Part> {
    let (bonded, pad_banks) = parse_device(device, series0).context("device")?;
    let em2 = parse_extension(extension, series0).context("deviceextension")?;
    for p in &em2 {
        ensure!(bonded.contains(p), "EM2 pin {} is not bonded", pin_name(*p, series0));
    }
    let mut part = Part {
        name: name.to_owned(),
        bonded,
        em2,
        ..Default::default()
    };

    let mut reader = Reader::from_str(portio);
    let mut module: Option<(String, Option<String>)> = None;
    let mut selector: Option<Option<String>> = None;
    let mut route: Option<(String, PartSignal)> = None;
    loop {
        let event = reader.read_event()?;
        let (end, e) = match &event {
            Event::Eof => break,
            Event::Start(e) => (false, e.to_owned()),
            Event::Empty(e) => (true, e.to_owned()),
            Event::End(e) => {
                match e.name().as_ref() {
                    b"module" => module = None,
                    b"selector" => selector = None,
                    b"route" => finish_route(&mut part, &module, route.take())?,
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        match e.name().as_ref() {
            b"module" => {
                let m = req(e.attributes(), "name", "module")?;
                // `PRS.ASYNCH0` is peripheral `PRS`, signal `ASYNCH0`.
                let (periph, sig) = match m.split_once('.') {
                    Some((p, s)) => (p.to_owned(), Some(s.to_owned())),
                    None => (m, None),
                };
                part.peripherals.insert(periph.clone());
                module = Some((periph, sig));
            }
            b"selector" => {
                ensure!(module.is_some(), "<selector> outside <module>");
                selector = Some(attr(e.attributes(), "locationPropertyId")?);
            }
            b"route" => {
                let Some(sel) = &selector else {
                    bail!("<route> outside <selector>")
                };
                let sig = req(e.attributes(), "name", "route")?;
                let bus = attr(e.attributes(), "aportName")?.filter(|a| !a.is_empty());
                route = Some((
                    sig,
                    PartSignal {
                        fixed: sel.is_none(),
                        bus,
                        locations: BTreeSet::new(),
                    },
                ));
                if end {
                    finish_route(&mut part, &module, route.take())?;
                }
            }
            b"location" => {
                let Some((sig, data)) = &mut route else {
                    bail!("<location> outside <route>")
                };
                let number: u16 = req(e.attributes(), "number", "location")?.parse()?;
                let bank: u8 = req(e.attributes(), "portBankIndex", "location")?.parse()?;
                let index: u8 = req(e.attributes(), "pinIndex", "location")?.parse()?;
                let periph = &module.as_ref().unwrap().0;
                if let Some(pad) = pad_banks.get(&bank) {
                    part.pads.insert(format!("{periph} {sig} {pad}"));
                } else {
                    ensure!(
                        part.bonded.contains(&(bank, index)),
                        "{periph} {sig}: location {number} is on pin {}, which is not bonded",
                        pin_name((bank, index), series0)
                    );
                    data.locations.insert((number, (bank, index)));
                }
            }
            _ => {}
        }
    }
    Ok(part)
}

fn finish_route(
    part: &mut Part,
    module: &Option<(String, Option<String>)>,
    route: Option<(String, PartSignal)>,
) -> Result<()> {
    let Some((sig, data)) = route else { return Ok(()) };
    let (periph, module_sig) = module.as_ref().ok_or_else(|| anyhow!("<route> outside <module>"))?;
    if let Some(ms) = module_sig {
        ensure!(ms == &sig, "module {periph}.{ms} has route {sig}");
    }
    let key = (periph.clone(), sig);
    ensure!(
        !part.signals.contains_key(&key),
        "{} {} is in two selectors",
        key.0,
        key.1
    );
    part.signals.insert(key, data);
    Ok(())
}

/// The `(number, pin)` pairs of a part's signal, in the form that
/// [`Signal::filter`] returns. The number is the location of a Series 0
/// routed signal and the alternative of a fixed signal. A Series 2 DBUS
/// location number is only an index, so it becomes 0.
fn part_pairs(series0: bool, s: &PartSignal) -> BTreeSet<(u8, PinId)> {
    let numbered = s.fixed || series0;
    s.locations
        .iter()
        .map(|&(n, p)| (if numbered { n as u8 } else { 0 }, p))
        .collect()
}

/// The number-to-pin map of a signal over all parts. Each number must give
/// the same pin on every part.
fn numbered_pins(periph: &str, sig: &str, series0: bool, with: &[(&Part, &PartSignal)]) -> Result<BTreeMap<u8, PinId>> {
    let mut out: BTreeMap<u8, PinId> = BTreeMap::new();
    for (p, s) in with {
        for &(n, pin) in &s.locations {
            let n = u8::try_from(n).map_err(|_| anyhow!("{periph} {sig}: number {n} > 255"))?;
            if let Some(old) = out.insert(n, pin) {
                ensure!(
                    old == pin,
                    "{periph} {sig}: number {n} is {} on {} but {} elsewhere",
                    pin_name(pin, series0),
                    p.name,
                    pin_name(old, series0)
                );
            }
        }
    }
    Ok(out)
}

/// Build the family table from its parts, and check that every part
/// round-trips.
///
/// `routed` gives the Series 2 GPIO route register of a peripheral signal,
/// when the GPIO has one. Such a signal must be whole ports on every part.
pub fn build_family(
    series0: bool,
    parts: &[Part],
    routed: &dyn Fn(&str, &str) -> Result<Option<String>>,
) -> Result<Family> {
    ensure!(!parts.is_empty(), "no parts");
    let mut keys: BTreeSet<&(String, String)> = BTreeSet::new();
    for p in parts {
        keys.extend(p.signals.keys());
    }

    let mut family = Family::default();
    for key in keys {
        let (periph, sig) = key;
        let with: Vec<(&Part, &PartSignal)> = parts
            .iter()
            .filter_map(|p| p.signals.get(key).map(|s| (p, s)))
            .collect();
        let (_, first) = with[0];
        for (p, s) in &with {
            ensure!(
                s.bus == first.bus && s.fixed == first.fixed,
                "{periph} {sig}: {} and {} differ in bus or location property",
                with[0].0.name,
                p.name
            );
        }
        let union: BTreeSet<PinId> = with.iter().flat_map(|(_, s)| s.locations.iter().map(|l| l.1)).collect();
        // A fixed selector with numbered locations has numbered alternatives
        // (DAC0 `OUT0ALT`). Else all numbers are 0.
        let numbered = with.iter().any(|(_, s)| s.locations.iter().any(|l| l.0 != 0));
        let route_register = if series0 { None } else { routed(periph, sig)? };
        let signal = if first.fixed {
            if let Some(reg) = &route_register {
                bail!("{periph} {sig}: the selector has fixed pins, but the GPIO has the route register `{reg}`");
            }
            if numbered {
                Signal::Alternatives(numbered_pins(periph, sig, series0, &with)?)
            } else {
                Signal::Pins(union)
            }
        } else if series0 {
            ensure!(first.bus.is_none(), "{periph} {sig}: a bus on Series 0");
            Signal::Locations(numbered_pins(periph, sig, true, &with)?)
        } else if let Some(bus) = &first.bus {
            Signal::Bus(bus.clone())
        } else {
            // The port set comes from the part with the most bonded pins.
            let (_, big_sig) = with
                .iter()
                .max_by_key(|(p, _)| (p.bonded.len(), std::cmp::Reverse(&p.name)))
                .unwrap();
            let ports: BTreeSet<u8> = big_sig.locations.iter().map(|l| l.1.0).collect();
            let candidate = Signal::Ports(ports);
            let whole_ports = with
                .iter()
                .all(|(p, s)| candidate.filter(&p.bonded) == part_pairs(false, s));
            if whole_ports {
                candidate
            } else if let Some(reg) = &route_register {
                // A routed signal that is not whole ports needs a new form.
                // Do not hide it as fixed pins.
                let detail: Vec<String> = with
                    .iter()
                    .filter(|(p, s)| candidate.filter(&p.bonded) != part_pairs(false, s))
                    .take(3)
                    .map(|(p, s)| {
                        let pins: Vec<String> = s.locations.iter().map(|l| pin_name(l.1, false)).collect();
                        format!("{} [{}]", p.name, pins.join(" "))
                    })
                    .collect();
                bail!(
                    "{periph} {sig}: the GPIO has the route register `{reg}`, but the pins are not whole ports: {}",
                    detail.join("; ")
                );
            } else {
                Signal::Pins(union)
            }
        };
        family
            .peripherals
            .entry(periph.clone())
            .or_default()
            .insert(sig.clone(), signal);
    }

    if series0 {
        let mut groups: BTreeMap<&BTreeSet<PinId>, Vec<String>> = BTreeMap::new();
        for p in parts {
            groups.entry(&p.bonded).or_default().push(p.name.clone());
        }
        let mut packages: Vec<Package> = groups
            .into_iter()
            .map(|(pins, mut parts)| {
                parts.sort();
                Package {
                    parts,
                    pins: pins.clone(),
                }
            })
            .collect();
        packages.sort_by(|a, b| a.parts.cmp(&b.parts));
        family.packages = packages;
    } else {
        let all_ports: BTreeSet<u8> = parts.iter().flat_map(|p| p.bonded.iter().map(|b| b.0)).collect();
        let ports: BTreeSet<u8> = all_ports
            .into_iter()
            .filter(|&port| {
                parts
                    .iter()
                    .all(|p| p.bonded.iter().filter(|b| b.0 == port).all(|b| p.em2.contains(b)))
            })
            .collect();
        let pins: BTreeSet<PinId> = parts
            .iter()
            .flat_map(|p| p.em2.iter().copied())
            .filter(|b| !ports.contains(&b.0))
            .collect();
        family.em2 = Some(Em2 { ports, pins });
    }

    round_trip(&family, parts)?;
    Ok(family)
}

/// Check each part against the family table filtered by its bonded pins.
pub fn round_trip(family: &Family, parts: &[Part]) -> Result<()> {
    let series0 = family.series0();
    let name = |p: &PinId| pin_name(*p, series0);
    // Show `location:pin` for a `locations` signal, else only the pin.
    let show = |set: &BTreeSet<(u8, PinId)>, located: bool| -> String {
        set.iter()
            .map(|(n, p)| if located { format!("{n}:{}", name(p)) } else { name(p) })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut errors = Vec::new();
    for part in parts {
        for (periph, signals) in &family.peripherals {
            if !part.peripherals.contains(periph) {
                continue;
            }
            for (sig, signal) in signals {
                let expected = signal.filter(&part.bonded);
                let actual = part
                    .signals
                    .get(&(periph.clone(), sig.clone()))
                    .map(|s| part_pairs(series0, s))
                    .unwrap_or_default();
                if expected != actual {
                    let located = matches!(signal, Signal::Locations(_) | Signal::Alternatives(_));
                    errors.push(format!(
                        "{}: {periph} {sig}: table gives [{}] but the part has [{}] (not in table [{}], not on part [{}])",
                        part.name,
                        show(&expected, located),
                        show(&actual, located),
                        show(&actual.difference(&expected).copied().collect(), located),
                        show(&expected.difference(&actual).copied().collect(), located),
                    ));
                }
            }
        }
        for key in part.signals.keys() {
            if !family.peripherals.get(&key.0).is_some_and(|s| s.contains_key(&key.1)) {
                errors.push(format!("{}: {} {} is not in the table", part.name, key.0, key.1));
            }
        }
        if let Some(em2) = &family.em2 {
            let expected: BTreeSet<PinId> = part.bonded.iter().copied().filter(|&p| em2.contains(p)).collect();
            if expected != part.em2 {
                errors.push(format!(
                    "{}: em2 table gives [{}] but the part has [{}]",
                    part.name,
                    expected.iter().map(name).collect::<Vec<_>>().join(" "),
                    part.em2.iter().map(name).collect::<Vec<_>>().join(" "),
                ));
            }
        }
        if let Some(pkg) = family.packages.iter().find(|p| p.parts.contains(&part.name)) {
            if pkg.pins != part.bonded {
                errors.push(format!("{}: package pins differ from the bonded pins", part.name));
            }
        } else if series0 {
            errors.push(format!("{}: in no package", part.name));
        }
    }
    if !errors.is_empty() {
        bail!("{} round-trip mismatch(es):\n  {}", errors.len(), errors.join("\n  "));
    }
    Ok(())
}

/// `AF_<PERIPHERAL>_<SIGNAL>` to location to pin, from the Series 0
/// `<family>_af_ports.h` and `<family>_af_pins.h` headers (Zlib).
pub fn parse_af_headers(ports_h: &str, pins_h: &str) -> Result<BTreeMap<String, BTreeMap<u8, PinId>>> {
    let define = regex::Regex::new(r"(?m)^#define\s+AF_([A-Za-z0-9_]+)_(PORT|PIN)\(i\)\s+\((.*?)\s*-1\)").unwrap();
    let arm = regex::Regex::new(r"\(i\) == (\d+) \? (\d+) :").unwrap();
    let table = |text: &str, kind: &str| -> Result<BTreeMap<String, BTreeMap<u8, u8>>> {
        let mut out: BTreeMap<String, BTreeMap<u8, u8>> = BTreeMap::new();
        for c in define.captures_iter(text) {
            if &c[2] != kind {
                continue;
            }
            let mut m = BTreeMap::new();
            for a in arm.captures_iter(&c[3]) {
                m.insert(a[1].parse()?, a[2].parse()?);
            }
            if let Some(old) = out.insert(c[1].to_owned(), m.clone()) {
                ensure!(old == m, "AF_{}_{kind} is defined twice", &c[1]);
            }
        }
        Ok(out)
    };
    let ports = table(ports_h, "PORT")?;
    let pins = table(pins_h, "PIN")?;
    ensure!(
        ports.keys().eq(pins.keys()),
        "the AF port and pin headers name different signals"
    );
    let mut out = BTreeMap::new();
    for (name, port_map) in ports {
        let pin_map = &pins[&name];
        ensure!(
            port_map.keys().eq(pin_map.keys()),
            "AF_{name}: port and pin locations differ"
        );
        out.insert(
            name,
            port_map.iter().map(|(&n, &port)| (n, (port, pin_map[&n]))).collect(),
        );
    }
    Ok(out)
}

/// Check every Series 0 `locations` row against the AF headers.
pub fn check_af(family: &Family, af: &BTreeMap<String, BTreeMap<u8, PinId>>) -> Result<()> {
    let mut errors = Vec::new();
    for (periph, signals) in &family.peripherals {
        for (sig, signal) in signals {
            let Signal::Locations(locs) = signal else { continue };
            let key = format!("{periph}_{sig}");
            let Some(header) = af.get(&key) else {
                errors.push(format!("{periph} {sig}: no AF_{key} macros"));
                continue;
            };
            for (n, pin) in locs {
                match header.get(n) {
                    Some(h) if h == pin => {}
                    Some(h) => errors.push(format!(
                        "{periph} {sig}: location {n} is {} in the pin tool but {} in the header",
                        pin_name(*pin, true),
                        pin_name(*h, true)
                    )),
                    None => errors.push(format!("{periph} {sig}: location {n} is not in the header")),
                }
            }
            for (n, h) in header {
                if !locs.contains_key(n) {
                    errors.push(format!(
                        "{periph} {sig}: header location {n} ({}) is on no part in the pin tool",
                        pin_name(*h, true)
                    ));
                }
            }
        }
    }
    if !errors.is_empty() {
        bail!("pin tool and AF headers disagree:\n  {}", errors.join("\n  "));
    }
    Ok(())
}

/// Inputs of one `extract-pins` run for one family.
pub struct ExtractInput<'a> {
    /// The family directory in the pin tool, for example `.../pin_tool/efr32mg24`.
    pub family_dir: &'a Path,
    /// Pin-tool version for the file header, for example `Simplicity SDK 2025.12.0`.
    pub sdk: &'a str,
    /// Chip JSON files of the family: lowercase name to chip.
    pub chips: &'a BTreeMap<String, crate::chips::ChipFile>,
    /// Extracted CMSIS packs, for the Series 0 AF headers.
    pub pack_dir: Option<&'a Path>,
    /// Curated register YAMLs, for the Series 2 GPIO route registers.
    pub registers_dir: &'a Path,
}

/// Extract one family. Returns the YAML text and a short report.
pub fn extract_family(input: &ExtractInput) -> Result<(String, Vec<String>)> {
    let family = input
        .family_dir
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| anyhow!("bad family dir {}", input.family_dir.display()))?
        .to_owned();
    // The chip JSON names the family exactly (`EFM32GG`), so `efm32g` does
    // not take the `efm32gg` chips.
    let chips: Vec<(&String, &crate::chips::ChipFile)> = input
        .chips
        .iter()
        .filter(|(_, c)| c.chip.family.eq_ignore_ascii_case(&family))
        .collect();
    ensure!(!chips.is_empty(), "{family}: no chip JSON with this family");
    let series = chips[0].1.chip.series.context("chip series missing")?.series;
    ensure!(
        chips
            .iter()
            .all(|(_, c)| c.chip.series.map(|s| s.series) == Some(series)),
        "{family}: chips of more than one series"
    );
    let series0 = series == 0;
    ensure!(series == 0 || series == 2, "{family}: Series {series} is not supported");

    let mut report = Vec::new();
    let mut parts = Vec::new();
    let mut missing = Vec::new();
    for (name, chip) in &chips {
        let dir = input.family_dir.join(name.as_str());
        if !dir.is_dir() {
            missing.push(name.to_string());
            continue;
        }
        let upper = name.to_ascii_uppercase();
        let read = |f: String| -> Result<String> {
            let path = dir.join(&f);
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
        };
        let part = parse_part(
            name,
            series0,
            &read(format!("{upper}.device"))?,
            &read(format!("{upper}.deviceextension"))?,
            &read("PORTIO.portio".to_owned())?,
        )
        .with_context(|| format!("part {name}"))?;
        if !series0 {
            let header: BTreeSet<PinId> = chip.pins.iter().map(|p| (p.port, p.pin)).collect();
            ensure!(
                header == part.bonded,
                "{name}: the pin tool bonds [{}] but the device header masks give [{}]",
                part.bonded
                    .iter()
                    .map(|p| pin_name(*p, false))
                    .collect::<Vec<_>>()
                    .join(" "),
                header.iter().map(|p| pin_name(*p, false)).collect::<Vec<_>>().join(" ")
            );
        }
        parts.push(part);
    }
    ensure!(!parts.is_empty(), "{family}: no chip has a pin-tool part");
    for m in &missing {
        report.push(format!("{m}: no pin-tool part, so no peripheral pins"));
    }
    let pads: BTreeSet<String> = parts.iter().flat_map(|p| p.pads.iter().cloned()).collect();
    if !pads.is_empty() {
        let parts_with: Vec<&str> = parts
            .iter()
            .filter(|p| !p.pads.is_empty())
            .map(|p| p.name.as_str())
            .collect();
        report.push(format!(
            "dropped {} non-GPIO pad location(s) on {} part(s): {}",
            pads.len(),
            parts_with.len(),
            pads.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    // The Series 2 GPIO register YAML, to find the route registers.
    let gpio_versions: BTreeSet<&str> = chips
        .iter()
        .flat_map(|(_, c)| c.peripherals.iter().filter(|p| p.kind == "gpio"))
        .map(|p| p.register_version.as_str())
        .collect();
    let mut regs = crate::pins::RegisterCache::new(input.registers_dir);
    let gpio = if series0 {
        None
    } else {
        ensure!(
            gpio_versions.len() == 1,
            "{family}: the chips have GPIO versions {gpio_versions:?}, not one"
        );
        let version = *gpio_versions.iter().next().unwrap();
        Some((version, regs.get(&format!("gpio_{version}"))?))
    };
    let routed = |periph: &str, sig: &str| -> Result<Option<String>> {
        Ok(gpio.and_then(|(version, names)| crate::pins::route_register(names, version, periph, sig)))
    };
    let table = build_family(series0, &parts, &routed).with_context(|| format!("family {family}"))?;

    // A part without a peripheral's `<module>` is not round-tripped for it.
    // Then the chip must not have that peripheral either, or `gen` would
    // give it rows that the pin tool does not list.
    let mut errors = Vec::new();
    for part in &parts {
        let owners = crate::pins::owners(&input.chips[&part.name]);
        for (periph, signals) in &table.peripherals {
            if part.peripherals.contains(periph) || crate::pins::chip_peripheral(&owners, periph).is_none() {
                continue;
            }
            if signals.values().any(|s| !s.filter(&part.bonded).is_empty()) {
                errors.push(format!(
                    "{}: the chip has {periph}, but its pin-tool part has no {periph} module",
                    part.name
                ));
            }
        }
    }
    ensure!(errors.is_empty(), "{family}:\n  {}", errors.join("\n  "));

    if series0 {
        let pack_dir = input.pack_dir.context("Series 0 needs --pack-dir for the AF headers")?;
        let upper = family.to_ascii_uppercase();
        let include = std::fs::read_dir(pack_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .map(|p| p.join("Device/SiliconLabs").join(&upper).join("Include"))
            .find(|p| p.join(format!("{family}_af_pins.h")).is_file())
            .with_context(|| format!("no {family}_af_pins.h under {}", pack_dir.display()))?;
        let ports_h = std::fs::read_to_string(include.join(format!("{family}_af_ports.h")))?;
        let pins_h = std::fs::read_to_string(include.join(format!("{family}_af_pins.h")))?;
        let af = parse_af_headers(&ports_h, &pins_h)?;
        check_af(&table, &af).with_context(|| format!("family {family}"))?;
        report.push(format!(
            "all location rows match {family}_af_ports.h and {family}_af_pins.h"
        ));
    }

    let mut forms: BTreeMap<&str, usize> = BTreeMap::new();
    for signals in table.peripherals.values() {
        for s in signals.values() {
            let f = match s {
                Signal::Ports(_) => "ports",
                Signal::Pins(_) => "pins",
                Signal::Alternatives(_) => "alternatives",
                Signal::Bus(_) => "bus",
                Signal::Locations(_) => "locations",
            };
            *forms.entry(f).or_default() += 1;
        }
    }
    report.insert(
        0,
        format!(
            "{} parts, {} peripherals, signals by form {forms:?}{}",
            parts.len(),
            table.peripherals.len(),
            if series0 {
                format!(", {} packages", table.packages.len())
            } else {
                String::new()
            }
        ),
    );

    let header = vec![
        format!(
            "{} pin facts. Extracted once from the Silicon Labs pin tool",
            family.to_ascii_uppercase()
        ),
        format!("({}) with `./d extract-pins`. Maintained by hand.", input.sdk),
    ];
    let yaml = table.to_yaml(&header);
    let back = Family::parse(&yaml).context("re-read the written YAML")?;
    ensure!(
        back == table,
        "{family}: the written YAML does not read back to the same table"
    );
    Ok((yaml, report))
}

#[cfg(test)]
mod tests {
    //! The XML here is written by hand, with made-up values. It has only the
    //! elements and attributes that the parser reads, in the structure that
    //! the module doc describes.

    use super::*;

    fn device(pins: &[&str], pads: &[&str]) -> String {
        let mut banks: BTreeMap<u8, Vec<(u8, &str)>> = BTreeMap::new();
        for p in pins {
            let id = (p.as_bytes()[1] - b'A', p[2..].parse::<u8>().unwrap());
            banks.entry(id.0).or_default().push((id.1, p));
        }
        let mut s = String::from("<root>");
        for (b, list) in banks {
            s += &format!("<portBank index=\"{b}\">");
            for (i, n) in list {
                s += &format!("<pin name=\"{n}\" index=\"{i}\"/>");
            }
            s += "</portBank>";
        }
        for (i, pad) in pads.iter().enumerate() {
            s += &format!(
                "<portBank index=\"{}\"><pin name=\"{pad}\" index=\"0\"/></portBank>",
                8 + i
            );
        }
        s + "</root>"
    }

    fn extension(em2: &[&str]) -> String {
        let mut s = String::from(
            "<root><componentExtension componentName=\"UART9\"><boolProperty id=\"capability.x\" defaultValue=\"true\"/></componentExtension>",
        );
        for p in em2 {
            s += &format!(
                "<componentExtension componentName=\"{p}\"><boolProperty id=\"capability.em2\" defaultValue=\"true\"/></componentExtension>"
            );
        }
        s + "</root>"
    }

    /// No signal has a GPIO route register.
    fn no_routes(_: &str, _: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// `(module, located, aport, route, [(number, bank, pin)])`.
    type Sel<'a> = (&'a str, bool, &'a str, &'a str, &'a [(u16, u8, u8)]);

    fn portio(selectors: &[Sel]) -> String {
        let mut s = String::from("<root>");
        for (module, located, aport, route, locs) in selectors {
            let lp = if *located {
                " locationPropertyId=\"x.location\""
            } else {
                ""
            };
            s += &format!("<module name=\"{module}\"><selector{lp}><route name=\"{route}\" aportName=\"{aport}\">");
            for (n, b, p) in *locs {
                s += &format!("<location number=\"{n}\" portBankIndex=\"{b}\" pinIndex=\"{p}\"/>");
            }
            s += "</route></selector></module>";
        }
        s + "</root>"
    }

    /// Two made-up Series 2 parts: `big` bonds PA00-PA02, PB00, PC00-PC01.
    /// `small` bonds PA00-PA01 and PC00.
    fn series2_parts(small_tx: &[(u16, u8, u8)]) -> Vec<Part> {
        let big_pins = ["PA00", "PA01", "PA02", "PB00", "PC00", "PC01"];
        let small_pins = ["PA00", "PA01", "PC00"];
        let big = portio(&[
            ("UART9", true, "", "TX", &[(0, 0, 0), (1, 0, 1), (2, 0, 2), (16, 1, 0)]),
            ("UART9", false, "", "WAKE", &[(0, 2, 1)]),
            (
                "ADC9",
                true,
                "BUS9",
                "IN",
                &[
                    (0, 0, 0),
                    (1, 0, 1),
                    (2, 0, 2),
                    (3, 1, 0),
                    (4, 2, 0),
                    (5, 2, 1),
                    (9, 8, 0),
                ],
            ),
            ("PRS.CH7", true, "", "CH7", &[(0, 2, 0), (1, 2, 1)]),
        ]);
        let small = portio(&[
            ("UART9", true, "", "TX", small_tx),
            ("ADC9", true, "BUS9", "IN", &[(0, 0, 0), (1, 0, 1), (4, 2, 0)]),
            ("PRS.CH7", true, "", "CH7", &[(0, 2, 0)]),
        ]);
        vec![
            parse_part(
                "big",
                false,
                &device(&big_pins, &["XIN0"]),
                &extension(&["PA00", "PA01", "PA02", "PC01"]),
                &big,
            )
            .unwrap(),
            parse_part(
                "small",
                false,
                &device(&small_pins, &[]),
                &extension(&["PA00", "PA01"]),
                &small,
            )
            .unwrap(),
        ]
    }

    #[test]
    fn classifies_ports_pins_and_bus() {
        let parts = series2_parts(&[(0, 0, 0), (1, 0, 1)]);
        assert_eq!(parts[0].pads, ["ADC9 IN XIN0".to_owned()].into());
        let f = build_family(false, &parts, &no_routes).unwrap();
        let uart = &f.peripherals["UART9"];
        assert_eq!(uart["TX"], Signal::Ports([0, 1].into()));
        assert_eq!(uart["WAKE"], Signal::Pins([(2, 1)].into()));
        assert_eq!(f.peripherals["ADC9"]["IN"], Signal::Bus("BUS9".into()));
        // The module `PRS.CH7` is peripheral `PRS`, signal `CH7`.
        assert_eq!(f.peripherals["PRS"]["CH7"], Signal::Ports([2].into()));
        assert_eq!(
            f.em2,
            Some(Em2 {
                ports: [0].into(),
                pins: [(2, 1)].into()
            })
        );
        let yaml = f.to_yaml(&[]);
        assert!(yaml.contains("    TX: { ports: [A, B] }\n"), "{yaml}");
        assert!(yaml.contains("    WAKE: { pins: [PC01] }\n"), "{yaml}");
        assert!(yaml.contains("    IN: { bus: BUS9 }\n"), "{yaml}");
    }

    #[test]
    fn series0_locations_filtered_by_bonded_pins() {
        // Location 3 is PC15. Part `p64` does not bond it, so its pin tool
        // data has no location 3. The family table has it.
        let full = portio(&[
            (
                "UART9",
                true,
                "",
                "TX",
                &[(0, 4, 10), (1, 4, 7), (2, 2, 11), (3, 2, 15)],
            ),
            ("UART9", false, "", "CH0", &[(0, 3, 0)]),
        ]);
        let small = portio(&[
            ("UART9", true, "", "TX", &[(0, 4, 10), (1, 4, 7), (2, 2, 11)]),
            ("UART9", false, "", "CH0", &[(0, 3, 0)]),
        ]);
        let ext = extension(&[]);
        let parts = vec![
            parse_part(
                "p100",
                true,
                &device(&["PC11", "PC15", "PD0", "PE7", "PE10"], &[]),
                &ext,
                &full,
            )
            .unwrap(),
            parse_part("p64", true, &device(&["PC11", "PD0", "PE7", "PE10"], &[]), &ext, &small).unwrap(),
        ];
        let f = build_family(true, &parts, &no_routes).unwrap();
        assert_eq!(
            f.peripherals["UART9"]["TX"],
            Signal::Locations([(0, (4, 10)), (1, (4, 7)), (2, (2, 11)), (3, (2, 15))].into())
        );
        assert_eq!(f.peripherals["UART9"]["CH0"], Signal::Pins([(3, 0)].into()));
        assert_eq!(f.packages.len(), 2);
        assert!(f.em2.is_none());
        let yaml = f.to_yaml(&[]);
        assert!(
            yaml.contains("    TX: { locations: { 0: PE10, 1: PE7, 2: PC11, 3: PC15 } }\n"),
            "{yaml}"
        );

        let af = parse_af_headers(
            "#define AF_UART9_TX_PORT(i)  ((i) == 0 ? 4 : (i) == 1 ? 4 : (i) == 2 ? 2 : (i) == 3 ? 2 :  -1)  /**< x */\n",
            "#define AF_UART9_TX_PIN(i)   ((i) == 0 ? 10 : (i) == 1 ? 7 : (i) == 2 ? 11 : (i) == 3 ? 15 :  -1)\n",
        )
        .unwrap();
        check_af(&f, &af).unwrap();
        let bad = parse_af_headers(
            "#define AF_UART9_TX_PORT(i)  ((i) == 0 ? 4 : (i) == 1 ? 4 : (i) == 2 ? 2 : (i) == 3 ? 2 :  -1)\n",
            "#define AF_UART9_TX_PIN(i)   ((i) == 0 ? 10 : (i) == 1 ? 7 : (i) == 2 ? 11 : (i) == 3 ? 14 :  -1)\n",
        )
        .unwrap();
        let err = check_af(&f, &bad).unwrap_err().to_string();
        assert!(
            err.contains("location 3 is PC15 in the pin tool but PC14 in the header"),
            "{err}"
        );
    }

    #[test]
    fn round_trip_mismatch_is_an_error() {
        // `small` bonds PA00 and PA01 but routes TX only to PA00. Neither
        // `ports` nor the `pins` union round-trips on `small`, so the
        // extractor stops and names the part, signal and difference.
        let parts = series2_parts(&[(0, 0, 0)]);
        let err = format!("{:#}", build_family(false, &parts, &no_routes).unwrap_err());
        assert!(
            err.contains(
                "small: UART9 TX: table gives [PA00 PA01] but the part has [PA00] (not in table [], not on part [PA01])"
            ),
            "{err}"
        );

        // A hand-edited table that disagrees with a part is also an error.
        let good = series2_parts(&[(0, 0, 0), (1, 0, 1)]);
        let mut edited = build_family(false, &good, &no_routes).unwrap();
        edited
            .peripherals
            .get_mut("UART9")
            .unwrap()
            .insert("TX".into(), Signal::Ports([0].into()));
        let err = round_trip(&edited, &good).unwrap_err().to_string();
        assert!(err.contains("big: UART9 TX: table gives [PA00 PA01 PA02]"), "{err}");
    }
    #[test]
    fn routed_signal_must_be_whole_ports() {
        // `small` routes TX only to PA00, so TX is not whole ports. With a
        // GPIO route register for TX, the extractor stops. It does not fall
        // back to fixed pins.
        let parts = series2_parts(&[(0, 0, 0)]);
        let routed = |p: &str, s: &str| -> Result<Option<String>> {
            Ok((p == "UART9" && s == "TX").then(|| "uart9_txroute".to_owned()))
        };
        let err = format!("{:#}", build_family(false, &parts, &routed).unwrap_err());
        assert!(
            err.contains("UART9 TX: the GPIO has the route register `uart9_txroute`, but the pins are not whole ports: small [PA00]"),
            "{err}"
        );

        // A fixed selector with a route register is also an error.
        let parts = series2_parts(&[(0, 0, 0), (1, 0, 1)]);
        let routed = |p: &str, s: &str| -> Result<Option<String>> {
            Ok((p == "UART9" && s == "WAKE").then(|| "uart9_wakeroute".to_owned()))
        };
        let err = format!("{:#}", build_family(false, &parts, &routed).unwrap_err());
        assert!(
            err.contains(
                "UART9 WAKE: the selector has fixed pins, but the GPIO has the route register `uart9_wakeroute`"
            ),
            "{err}"
        );
    }

    #[test]
    fn fixed_alternatives_keep_their_number() {
        // A made-up DAC9 OUT0ALT with three numbered fixed alternatives. The
        // small part does not bond PC1, so it has only 0 and 2.
        let full = portio(&[("DAC9", false, "", "OUT0ALT", &[(0, 2, 0), (1, 2, 1), (2, 3, 0)])]);
        let small = portio(&[("DAC9", false, "", "OUT0ALT", &[(0, 2, 0), (2, 3, 0)])]);
        let ext = extension(&[]);
        let parts = vec![
            parse_part("p100", true, &device(&["PC0", "PC1", "PD0"], &[]), &ext, &full).unwrap(),
            parse_part("p64", true, &device(&["PC0", "PD0"], &[]), &ext, &small).unwrap(),
        ];
        let f = build_family(true, &parts, &no_routes).unwrap();
        assert_eq!(
            f.peripherals["DAC9"]["OUT0ALT"],
            Signal::Alternatives([(0, (2, 0)), (1, (2, 1)), (2, (3, 0))].into())
        );
        let yaml = f.to_yaml(&[]);
        assert!(
            yaml.contains("    OUT0ALT: { alternatives: { 0: PC0, 1: PC1, 2: PD0 } }\n"),
            "{yaml}"
        );
        assert_eq!(Family::parse(&yaml).unwrap(), f);

        // The round-trip compares the numbers, not only the pins.
        let mut edited = f.clone();
        edited.peripherals.get_mut("DAC9").unwrap().insert(
            "OUT0ALT".into(),
            Signal::Alternatives([(0, (2, 0)), (1, (2, 1)), (3, (3, 0))].into()),
        );
        let err = round_trip(&edited, &parts).unwrap_err().to_string();
        assert!(
            err.contains("p64: DAC9 OUT0ALT: table gives [0:PC0 3:PD0] but the part has [0:PC0 2:PD0]"),
            "{err}"
        );
    }
}
