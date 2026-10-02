//! Curated pin facts in `data/pins/<family>.yaml`, and the peripheral pin
//! rows that `gen` builds from them.
//!
//! The files were extracted once from the Silicon Labs pin tool (see
//! [`crate::pintool`]). After that, they are maintained by hand. `gen` reads
//! only these files. It never reads pin-tool files.
//!
//! A file has these keys:
//! - `em2` (Series 2 only): the EM2-capable pins, as whole `ports` plus
//!   single `pins`.
//! - `packages` (Series 0 only): the bonded pins of each group of parts.
//!   Series 2 takes its bonded pins from the device headers instead.
//! - `peripherals`: for each peripheral and signal, one of these forms:
//!   - `{ ports: [A, B] }`: every bonded pin of these ports.
//!   - `{ pins: [PD01] }`: these fixed pins, when they are bonded.
//!   - `{ alternatives: { 0: PC0, 1: PC1 } }`: numbered fixed alternatives,
//!     when the pin is bonded (Series 0 DAC0 `OUT0ALT`).
//!   - `{ bus: ABUS }`: every bonded pin, through this analog bus.
//!   - `{ locations: { 0: PE10, 1: PE7 } }` (Series 0): the ROUTE LOCATION
//!     values, when the pin is bonded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::chips::ChipFile;
#[cfg(test)]
use crate::chips::Pin;

/// A GPIO pin as `(port, pin)`. Port 0 is A.
pub type PinId = (u8, u8);

/// Vendor pin name: `PA05` on Series 2, `PA5` on Series 0.
pub fn pin_name(pin: PinId, series0: bool) -> String {
    let port = (b'A' + pin.0) as char;
    if series0 {
        format!("P{port}{}", pin.1)
    } else {
        format!("P{port}{:02}", pin.1)
    }
}

/// Parse a vendor pin name. The name must use the exact vendor format of the
/// series: `PA05` on Series 2, `PA5` on Series 0.
pub fn parse_pin(name: &str, series0: bool) -> Result<PinId> {
    let bytes = name.as_bytes();
    ensure!(
        bytes.len() >= 3 && bytes[0] == b'P' && bytes[1].is_ascii_uppercase(),
        "`{name}` is not a pin name"
    );
    let pin: u8 = name[2..].parse().map_err(|_| anyhow!("`{name}` is not a pin name"))?;
    let id = (bytes[1] - b'A', pin);
    ensure!(
        pin_name(id, series0) == name,
        "`{name}` does not use the Series {} pin format (`{}`)",
        if series0 { 0 } else { 2 },
        pin_name(id, series0)
    );
    Ok(id)
}

fn parse_port(name: &str) -> Result<u8> {
    let b = name.as_bytes();
    ensure!(
        b.len() == 1 && b[0].is_ascii_uppercase(),
        "`{name}` is not a port letter"
    );
    Ok(b[0] - b'A')
}

fn port_letter(port: u8) -> char {
    (b'A' + port) as char
}

/// The pins of one signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// Every bonded pin of these ports (DBUS routing on Series 2).
    Ports(BTreeSet<u8>),
    /// These fixed pins, when bonded.
    Pins(BTreeSet<PinId>),
    /// Numbered fixed alternatives, when bonded. The number selects the
    /// alternative in the peripheral (DAC0 `OUT0ALT` uses `OPA0MUX.OUTPEN`).
    Alternatives(BTreeMap<u8, PinId>),
    /// Every bonded pin, through this analog bus.
    Bus(String),
    /// Series 0 ROUTE LOCATION value to pin.
    Locations(BTreeMap<u8, PinId>),
}

impl Signal {
    /// The `(location, pin)` pairs this signal has on a part with the given
    /// bonded pins. The number is 0 for every form except `Locations` and
    /// `Alternatives`.
    pub fn filter(&self, bonded: &BTreeSet<PinId>) -> BTreeSet<(u8, PinId)> {
        match self {
            Signal::Ports(ports) => bonded
                .iter()
                .filter(|p| ports.contains(&p.0))
                .map(|&p| (0, p))
                .collect(),
            Signal::Pins(pins) => pins.intersection(bonded).map(|&p| (0, p)).collect(),
            Signal::Bus(_) => bonded.iter().map(|&p| (0, p)).collect(),
            Signal::Locations(locs) | Signal::Alternatives(locs) => locs
                .iter()
                .filter(|(_, p)| bonded.contains(p))
                .map(|(&n, &p)| (n, p))
                .collect(),
        }
    }
}

/// EM2-capable pins (Series 2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Em2 {
    /// Every bonded pin of these ports is EM2-capable.
    pub ports: BTreeSet<u8>,
    /// More EM2-capable pins, outside `ports`.
    pub pins: BTreeSet<PinId>,
}

impl Em2 {
    pub fn contains(&self, pin: PinId) -> bool {
        self.ports.contains(&pin.0) || self.pins.contains(&pin)
    }
}

/// Bonded pins of a group of Series 0 parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Lowercase part names.
    pub parts: Vec<String>,
    pub pins: BTreeSet<PinId>,
}

/// The pin facts of one family, as in `data/pins/<family>.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Family {
    pub em2: Option<Em2>,
    pub packages: Vec<Package>,
    /// Peripheral name to signal name to pins.
    pub peripherals: BTreeMap<String, BTreeMap<String, Signal>>,
}

impl Family {
    /// A family with `packages` uses the Series 0 pin format.
    pub fn series0(&self) -> bool {
        !self.packages.is_empty()
    }

    /// Parse the YAML text of one family file.
    pub fn parse(text: &str) -> Result<Family> {
        let raw: RawFamily = serde_yaml::from_str(text)?;
        let series0 = !raw.packages.is_empty();
        ensure!(
            !(series0 && raw.em2.is_some()),
            "a file has either `packages` (Series 0) or `em2` (Series 2), not both"
        );
        let pins = |names: &[String]| -> Result<BTreeSet<PinId>> {
            let mut out = BTreeSet::new();
            for n in names {
                ensure!(out.insert(parse_pin(n, series0)?), "pin {n} is listed twice");
            }
            Ok(out)
        };
        let ports = |names: &[String]| -> Result<BTreeSet<u8>> {
            let mut out = BTreeSet::new();
            for n in names {
                ensure!(out.insert(parse_port(n)?), "port {n} is listed twice");
            }
            Ok(out)
        };
        let em2 = raw
            .em2
            .map(|e| -> Result<Em2> {
                Ok(Em2 {
                    ports: ports(&e.ports)?,
                    pins: pins(&e.pins)?,
                })
            })
            .transpose()
            .context("em2")?;
        let mut packages = Vec::new();
        let mut seen_parts = BTreeSet::new();
        for p in &raw.packages {
            for part in &p.parts {
                ensure!(seen_parts.insert(part.clone()), "part {part} is in two packages");
            }
            packages.push(Package {
                parts: p.parts.clone(),
                pins: pins(&p.pins).with_context(|| format!("package {:?}", p.parts))?,
            });
        }
        let mut peripherals = BTreeMap::new();
        for (periph, signals) in raw.peripherals {
            let mut out = BTreeMap::new();
            for (sig, s) in signals {
                let ctx = || format!("{periph} {sig}");
                let n = s.ports.is_some() as u8
                    + s.pins.is_some() as u8
                    + s.alternatives.is_some() as u8
                    + s.bus.is_some() as u8
                    + s.locations.is_some() as u8;
                ensure!(
                    n == 1,
                    "{}: give exactly one of ports, pins, alternatives, bus, locations",
                    ctx()
                );
                let numbered = |m: BTreeMap<u8, String>| -> Result<BTreeMap<u8, PinId>> {
                    let mut out = BTreeMap::new();
                    for (n, p) in m {
                        out.insert(n, parse_pin(&p, series0).with_context(ctx)?);
                    }
                    Ok(out)
                };
                let signal = if let Some(p) = &s.ports {
                    ensure!(!series0, "{}: `ports` is a Series 2 form", ctx());
                    Signal::Ports(ports(p).with_context(ctx)?)
                } else if let Some(p) = &s.pins {
                    Signal::Pins(pins(p).with_context(ctx)?)
                } else if let Some(a) = s.alternatives {
                    Signal::Alternatives(numbered(a)?)
                } else if let Some(b) = s.bus {
                    Signal::Bus(b)
                } else {
                    ensure!(series0, "{}: `locations` is a Series 0 form", ctx());
                    Signal::Locations(numbered(s.locations.unwrap())?)
                };
                out.insert(sig, signal);
            }
            peripherals.insert(periph, out);
        }
        Ok(Family {
            em2,
            packages,
            peripherals,
        })
    }

    /// Render the family as YAML, after the given comment lines.
    pub fn to_yaml(&self, header: &[String]) -> String {
        let series0 = self.series0();
        let pins = |set: &BTreeSet<PinId>| -> String {
            set.iter().map(|&p| pin_name(p, series0)).collect::<Vec<_>>().join(", ")
        };
        let ports = |set: &BTreeSet<u8>| -> String {
            set.iter()
                .map(|&p| port_letter(p).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let numbered = |m: &BTreeMap<u8, PinId>| -> String {
            m.iter()
                .map(|(n, &p)| format!("{n}: {}", pin_name(p, series0)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut s = String::new();
        for line in header {
            writeln!(s, "# {line}").unwrap();
        }
        if let Some(em2) = &self.em2 {
            s.push_str("em2:\n");
            writeln!(s, "  ports: [{}]", ports(&em2.ports)).unwrap();
            writeln!(s, "  pins: [{}]", pins(&em2.pins)).unwrap();
        }
        if !self.packages.is_empty() {
            s.push_str("packages:\n");
            for p in &self.packages {
                writeln!(s, "  - parts: [{}]", p.parts.join(", ")).unwrap();
                // One line for each port keeps the long lists readable.
                let mut by_port: BTreeMap<u8, BTreeSet<PinId>> = BTreeMap::new();
                for &pin in &p.pins {
                    by_port.entry(pin.0).or_default().insert(pin);
                }
                let lines: Vec<String> = by_port.values().map(&pins).collect();
                writeln!(s, "    pins: [{}]", lines.join(",\n      ")).unwrap();
            }
        }
        s.push_str("peripherals:\n");
        for (periph, signals) in &self.peripherals {
            writeln!(s, "  {}:", yaml_key(periph)).unwrap();
            for (sig, signal) in signals {
                let body = match signal {
                    Signal::Ports(p) => format!("ports: [{}]", ports(p)),
                    Signal::Pins(p) => format!("pins: [{}]", pins(p)),
                    Signal::Alternatives(a) => format!("alternatives: {{ {} }}", numbered(a)),
                    Signal::Bus(b) => format!("bus: {}", yaml_key(b)),
                    Signal::Locations(l) => format!("locations: {{ {} }}", numbered(l)),
                };
                writeln!(s, "    {}: {{ {body} }}", yaml_key(sig)).unwrap();
            }
        }
        s
    }
}

/// Quote a name when a YAML reader could take it for something other than a
/// string (`N`, `ON`, `null`, ...).
fn yaml_key(name: &str) -> String {
    const SPECIAL: &[&str] = &["y", "n", "yes", "no", "on", "off", "true", "false", "null", "~"];
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain && !SPECIAL.contains(&name.to_ascii_lowercase().as_str()) {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFamily {
    #[serde(default)]
    em2: Option<RawEm2>,
    #[serde(default)]
    packages: Vec<RawPackage>,
    peripherals: BTreeMap<String, BTreeMap<String, RawSignal>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEm2 {
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    pins: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    parts: Vec<String>,
    pins: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignal {
    ports: Option<Vec<String>>,
    pins: Option<Vec<String>>,
    alternatives: Option<BTreeMap<u8, String>>,
    bus: Option<String>,
    locations: Option<BTreeMap<u8, String>>,
}

/// Load every `<family>.yaml` in `dir`. The key is the file stem. A missing
/// directory gives no families, so `gen` can run before the first
/// extraction.
pub fn load_dir(dir: &Path) -> Result<BTreeMap<String, Family>> {
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap().to_owned();
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let family = Family::parse(&text).with_context(|| format!("parse {}", path.display()))?;
        out.insert(stem, family);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Peripheral pin entries in the chip JSON.
// ---------------------------------------------------------------------------

/// The pins that can carry one signal of a peripheral with one route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeripheralPins {
    /// Signal name without the peripheral prefix: `TX`, `SDA`, `CC0`.
    pub signal: String,
    pub route: PinRoute,
    /// Vendor pin names (`PA05` on Series 2, `PA5` on Series 0), sorted by
    /// port, then by pin number.
    pub pins: Vec<String>,
}

/// How to connect a pin to the signal. The derived order sorts `Location`
/// by number, `Analog` by bus name and `Fixed` by alternative.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PinRoute {
    /// Series 2 digital bus: GPIO route register, and the enable bit.
    Dbus {
        register: String,
        enable: Option<PinRouteEnable>,
    },
    /// Series 0: the peripheral ROUTE LOCATION value, and the enable bit.
    Location {
        location: u8,
        enable: Option<PinRouteEnable>,
    },
    /// Series 2 analog bus of the pin's port. Allocate it in
    /// `GPIO.<bus>ALLOC`. The field depends on the pin parity.
    Analog { bus: String },
    /// Fixed-function pin. `enable` is the field that connects it, when the
    /// peripheral has one. `alternative` is the number of a numbered fixed
    /// alternative (DAC0 `OUT0ALT`).
    Fixed {
        enable: Option<PinRouteEnable>,
        alternative: Option<u8>,
    },
}

/// A register field that enables a routed pin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PinRouteEnable {
    pub register: String,
    pub field: String,
}

impl PinRouteEnable {
    fn new(register: &str, field: &str) -> Self {
        PinRouteEnable {
            register: register.to_owned(),
            field: field.to_owned(),
        }
    }
}

/// Register and field names of one `data/registers/<kind>_<version>.yaml`,
/// enough to resolve route registers.
#[derive(Debug, Default)]
pub struct RegisterNames {
    /// Register name to its field names.
    pub registers: BTreeMap<String, BTreeSet<String>>,
    /// Register-array name to its length.
    pub arrays: BTreeMap<String, u32>,
}

impl RegisterNames {
    pub fn parse(text: &str) -> Result<RegisterNames> {
        #[derive(Deserialize)]
        struct Item {
            name: String,
            #[serde(default)]
            fieldset: Option<String>,
            #[serde(default)]
            array: Option<Array>,
        }
        #[derive(Deserialize)]
        struct Array {
            #[serde(default)]
            len: Option<u32>,
        }
        #[derive(Deserialize)]
        struct Field {
            name: String,
        }
        #[derive(Deserialize)]
        struct Node {
            #[serde(default)]
            items: Vec<Item>,
            #[serde(default)]
            fields: Vec<Field>,
        }
        let top: BTreeMap<String, serde_yaml::Value> = serde_yaml::from_str(text)?;
        let mut fieldsets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut items: Vec<Item> = Vec::new();
        for (key, value) in top {
            if let Some(name) = key.strip_prefix("fieldset/") {
                let node: Node = serde_yaml::from_value(value)?;
                fieldsets.insert(name.to_owned(), node.fields.into_iter().map(|f| f.name).collect());
            } else if key.starts_with("block/") {
                let node: Node = serde_yaml::from_value(value)?;
                items.extend(node.items);
            }
        }
        let mut out = RegisterNames::default();
        for item in items {
            if let Some(len) = item.array.as_ref().and_then(|a| a.len) {
                out.arrays.insert(item.name.clone(), len);
            }
            let fields = item
                .fieldset
                .as_ref()
                .map(|f| fieldsets.get(f).cloned().unwrap_or_default())
                .unwrap_or_default();
            if let Some(old) = out.registers.insert(item.name.clone(), fields.clone()) {
                ensure!(old == fields, "register `{}` has two layouts", item.name);
            }
        }
        Ok(out)
    }

    fn has_field(&self, register: &str, field: &str) -> bool {
        self.registers.get(register).is_some_and(|f| f.contains(field))
    }
}

/// Cache of [`RegisterNames`], keyed by `<kind>_<version>`.
pub struct RegisterCache<'a> {
    dir: &'a Path,
    cache: BTreeMap<String, RegisterNames>,
}

impl<'a> RegisterCache<'a> {
    pub fn new(dir: &'a Path) -> Self {
        RegisterCache {
            dir,
            cache: BTreeMap::new(),
        }
    }

    pub fn get(&mut self, module: &str) -> Result<&RegisterNames> {
        if !self.cache.contains_key(module) {
            let path = self.dir.join(format!("{module}.yaml"));
            let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let names = RegisterNames::parse(&text).with_context(|| format!("parse {}", path.display()))?;
            self.cache.insert(module.to_owned(), names);
        }
        Ok(&self.cache[module])
    }

    /// Put a parsed module into the cache, for tests.
    #[cfg(test)]
    fn insert(&mut self, module: &str, names: RegisterNames) {
        self.cache.insert(module.to_owned(), names);
    }
}

/// A name pattern: a trailing `*` matches any suffix.
fn matches_glob(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

fn matches_any(patterns: &[&str], name: &str) -> bool {
    patterns.iter().any(|p| matches_glob(p, name))
}

/// Series 2 route registers whose names do not follow the rule
/// `GPIO.<inst>_<sig>route` and `GPIO.<inst>_routeen.<sig>pen`. `<inst>` and
/// `<sig>` are the lowercase peripheral and signal names. Every name this
/// table gives must exist in the GPIO register YAML, or `gen` stops.
///
/// | GPIO versions | Peripheral | Signal | Change | GPIO register YAML |
/// |---|---|---|---|---|
/// | s2v3, s2v4, s2v7 | `LETIMER0` | all | prefix `letimer` | `letimer_out0route` |
/// | all | `PRS` | all | prefix `prs0` | `prs0_asynch0route` |
/// | all | `TIMERn` | `CDTI0..2` | enable `ccc0pen..ccc2pen` | `timer0_routeen.ccc0pen` |
/// | s2v7 | `TIMER5..9` | `CDTI0..2` | route `ccc0route..ccc2route` | `timer5_ccc0route` |
/// | s2v7 | `USART1`, `USART2` | `CLK` | stem `sclk` | `usart1_sclkroute`, `usart1_routeen.sclkpen` |
/// | all | `ACMPn` | `DIGOUT` | stem `acmpout` | `acmp0_acmpoutroute` |
/// | all | `KEYSCAN` | all | no `_` in stem | `keyscan_colout0route` |
/// | all | `HFXO0` | `BUFOUT_REQ_IN_ASYNC` | prefix `syxo0`, no `_` in stem | `syxo0_bufoutreqinasyncroute` |
/// | all | `USB` | `USB_VBUS_SENSE` | no `_` in stem | `usb_usbvbussenseroute` |
const ROUTE_EXCEPTIONS: &[RouteException] = &[
    RouteException {
        gpio: &["s2v3", "s2v4", "s2v7"],
        prefix: Some("letimer"),
        ..RouteException::new(&["LETIMER0"], &[])
    },
    RouteException {
        prefix: Some("prs0"),
        ..RouteException::new(&["PRS"], &[])
    },
    RouteException {
        enable_replace: Some(("cdti", "ccc")),
        ..RouteException::new(&["TIMER*"], &["CDTI0", "CDTI1", "CDTI2"])
    },
    RouteException {
        gpio: &["s2v7"],
        route_replace: Some(("cdti", "ccc")),
        ..RouteException::new(
            &["TIMER5", "TIMER6", "TIMER7", "TIMER8", "TIMER9"],
            &["CDTI0", "CDTI1", "CDTI2"],
        )
    },
    RouteException {
        gpio: &["s2v7"],
        stem: Some("sclk"),
        ..RouteException::new(&["USART1", "USART2"], &["CLK"])
    },
    RouteException {
        stem: Some("acmpout"),
        ..RouteException::new(&["ACMP*"], &["DIGOUT"])
    },
    RouteException {
        strip_underscores: true,
        ..RouteException::new(&["KEYSCAN"], &[])
    },
    RouteException {
        prefix: Some("syxo0"),
        strip_underscores: true,
        ..RouteException::new(&["HFXO0"], &["BUFOUT_REQ_IN_ASYNC"])
    },
    RouteException {
        strip_underscores: true,
        ..RouteException::new(&["USB"], &["USB_VBUS_SENSE"])
    },
];

struct RouteException {
    /// GPIO register versions. Empty means all.
    gpio: &'static [&'static str],
    /// Peripheral names. A trailing `*` matches any suffix.
    peripherals: &'static [&'static str],
    /// Signal names. Empty means all.
    signals: &'static [&'static str],
    /// Register prefix instead of the lowercase peripheral name.
    prefix: Option<&'static str>,
    /// Stem instead of the lowercase signal name.
    stem: Option<&'static str>,
    /// Remove `_` from the stem (`COL_OUT_0` becomes `colout0`).
    strip_underscores: bool,
    /// Replace a part of the stem in the route register name only.
    route_replace: Option<(&'static str, &'static str)>,
    /// Replace a part of the stem in the enable field name only.
    enable_replace: Option<(&'static str, &'static str)>,
}

impl RouteException {
    const fn new(peripherals: &'static [&'static str], signals: &'static [&'static str]) -> Self {
        RouteException {
            gpio: &[],
            peripherals,
            signals,
            prefix: None,
            stem: None,
            strip_underscores: false,
            route_replace: None,
            enable_replace: None,
        }
    }

    fn matches(&self, gpio_version: &str, peripheral: &str, signal: &str) -> bool {
        matches_any(self.peripherals, peripheral)
            && (self.gpio.is_empty() || self.gpio.contains(&gpio_version))
            && (self.signals.is_empty() || self.signals.contains(&signal))
    }
}

/// The Series 2 GPIO register names of one signal, from the rule and the
/// [`ROUTE_EXCEPTIONS`] table. They need not exist.
struct DbusNames {
    register: String,
    enable_register: String,
    field: String,
    /// An exception names the enable field, so it must exist.
    enable_named: bool,
    /// Text for an error message.
    note: &'static str,
}

fn dbus_names(gpio_version: &str, peripheral: &str, signal: &str) -> DbusNames {
    let mut prefix = peripheral.to_ascii_lowercase();
    let mut stem = signal.to_ascii_lowercase();
    let mut route_stem: Option<String> = None;
    let mut enable_stem: Option<String> = None;
    let mut exception = false;
    for e in ROUTE_EXCEPTIONS
        .iter()
        .filter(|e| e.matches(gpio_version, peripheral, signal))
    {
        exception = true;
        if let Some(p) = e.prefix {
            prefix = p.to_owned();
        }
        if let Some(s) = e.stem {
            stem = s.to_owned();
        }
        if e.strip_underscores {
            stem = stem.replace('_', "");
        }
        if let Some((from, to)) = e.route_replace {
            route_stem = Some(stem.replace(from, to));
        }
        if let Some((from, to)) = e.enable_replace {
            enable_stem = Some(stem.replace(from, to));
        }
    }
    DbusNames {
        register: format!("{prefix}_{}route", route_stem.as_deref().unwrap_or(&stem)),
        enable_register: format!("{prefix}_routeen"),
        field: format!("{}pen", enable_stem.as_deref().unwrap_or(&stem)),
        enable_named: enable_stem.is_some(),
        note: if exception {
            " (with the route exception table)"
        } else {
            ""
        },
    }
}

/// The Series 2 GPIO route register of a signal, when the GPIO has one.
pub fn route_register(gpio: &RegisterNames, gpio_version: &str, peripheral: &str, signal: &str) -> Option<String> {
    let names = dbus_names(gpio_version, peripheral, signal);
    gpio.registers.contains_key(&names.register).then_some(names.register)
}

/// Resolve the Series 2 GPIO route register and enable bit of one signal.
fn dbus_route(gpio: &RegisterNames, gpio_version: &str, peripheral: &str, signal: &str) -> Result<PinRoute> {
    let n = dbus_names(gpio_version, peripheral, signal);
    ensure!(
        gpio.registers.contains_key(&n.register),
        "GPIO {gpio_version} has no route register `{}` for {peripheral} {signal}{}",
        n.register,
        n.note
    );
    let enable = if gpio.has_field(&n.enable_register, &n.field) {
        Some(PinRouteEnable::new(&n.enable_register, &n.field))
    } else {
        // An exception that names an enable field must find it.
        ensure!(
            !n.enable_named,
            "GPIO {gpio_version} has no `{}.{}` for {peripheral} {signal}{}",
            n.enable_register,
            n.field,
            n.note
        );
        None
    };
    Ok(PinRoute::Dbus {
        register: n.register,
        enable,
    })
}

/// The routed signals that have no enable field. `gen` stops when a `Dbus`
/// or `Location` signal has no enable and no entry here. It also stops when
/// a signal in this list has an enable, because then the entry is stale.
/// Signal names match the pin file. A trailing `*` matches any suffix.
///
/// | Series | Peripheral | Signals | Reason |
/// |---|---|---|---|
/// | 2 | `EUSARTn`, `USARTn`, `EUARTn` | `CTS` | input-only |
/// | 2 | `EUART0` | `RX` | input-only: `euart0_routeen` (s2v1) has no `rxpen` |
/// | 2 | `CMU` | `CLKIN0` | input-only clock input |
/// | 2 | `HFXO0` | `BUFOUT_REQ_IN_ASYNC` | input-only request input |
/// | 2 | `PCNTn` | `S0IN`, `S1IN` | input-only counter inputs |
/// | 2 | `KEYSCAN` | `ROW_SENSE_n` | input-only row inputs |
/// | 2 | `PDM` | `DAT0`, `DAT1` | input-only data inputs |
/// | 2 | `USB` | `USB_VBUS_SENSE` | input-only VBUS sense |
/// | 0 | `PCNTn` | `S0IN`, `S1IN` | input-only: `route` has only `location` |
/// | 0 | `EBI` | `A00..A27` | the multi-bit field `apen` (with `alb`) enables a range of address lines |
pub const NO_ENABLE: &[NoEnable] = &[
    NoEnable {
        series0: false,
        peripherals: &["EUSART*", "USART*", "EUART*"],
        signals: &["CTS"],
        reason: "input-only",
    },
    NoEnable {
        series0: false,
        peripherals: &["EUART0"],
        signals: &["RX"],
        reason: "input-only: `euart0_routeen` (s2v1) has no `rxpen`",
    },
    NoEnable {
        series0: false,
        peripherals: &["CMU"],
        signals: &["CLKIN0"],
        reason: "input-only clock input",
    },
    NoEnable {
        series0: false,
        peripherals: &["HFXO0"],
        signals: &["BUFOUT_REQ_IN_ASYNC"],
        reason: "input-only request input",
    },
    NoEnable {
        series0: false,
        peripherals: &["PCNT*"],
        signals: &["S0IN", "S1IN"],
        reason: "input-only counter inputs",
    },
    NoEnable {
        series0: false,
        peripherals: &["KEYSCAN"],
        signals: &["ROW_SENSE_*"],
        reason: "input-only row inputs",
    },
    NoEnable {
        series0: false,
        peripherals: &["PDM"],
        signals: &["DAT0", "DAT1"],
        reason: "input-only data inputs",
    },
    NoEnable {
        series0: false,
        peripherals: &["USB"],
        signals: &["USB_VBUS_SENSE"],
        reason: "input-only VBUS sense",
    },
    NoEnable {
        series0: true,
        peripherals: &["PCNT*"],
        signals: &["S0IN", "S1IN"],
        reason: "input-only: `route` has only `location`",
    },
    NoEnable {
        series0: true,
        peripherals: &["EBI"],
        signals: &[
            "A00", "A01", "A02", "A03", "A04", "A05", "A06", "A07", "A08", "A09", "A10", "A11", "A12", "A13", "A14",
            "A15", "A16", "A17", "A18", "A19", "A20", "A21", "A22", "A23", "A24", "A25", "A26", "A27",
        ],
        reason: "the multi-bit field `apen` (with `alb`) enables a range of address lines",
    },
];

/// One [`NO_ENABLE`] entry.
pub struct NoEnable {
    pub series0: bool,
    pub peripherals: &'static [&'static str],
    pub signals: &'static [&'static str],
    pub reason: &'static str,
}

fn no_enable_reason(series0: bool, peripheral: &str, signal: &str) -> Option<&'static str> {
    NO_ENABLE
        .iter()
        .find(|e| e.series0 == series0 && matches_any(e.peripherals, peripheral) && matches_any(e.signals, signal))
        .map(|e| e.reason)
}

/// Check a routed signal's enable against [`NO_ENABLE`].
fn check_enable(series0: bool, peripheral: &str, signal: &str, route: &PinRoute) -> Result<()> {
    let enable = match route {
        PinRoute::Dbus { enable, .. } | PinRoute::Location { enable, .. } => enable,
        _ => return Ok(()),
    };
    match (enable, no_enable_reason(series0, peripheral, signal)) {
        (None, None) => {
            anyhow::bail!("{peripheral} {signal}: the route has no enable field, and NO_ENABLE has no entry for it")
        }
        (Some(e), Some(reason)) => anyhow::bail!(
            "{peripheral} {signal}: NO_ENABLE says \"{reason}\", but the route has the enable `{}.{}`",
            e.register,
            e.field
        ),
        _ => Ok(()),
    }
}

/// Pin-file peripheral names that differ from the chip's peripheral name.
/// `gen` uses an entry only when the chip has no peripheral with the
/// pin-file name.
///
/// | Pin file | Chip | Why |
/// |---|---|---|
/// | `USB` | `USB_NS_APBS` | FG25: the USB core registers. This block owns the `USB` IRQ. |
const PERIPHERAL_NAMES: &[(&str, &str)] = &[("USB", "USB_NS_APBS")];

/// Series 0 ROUTE enable fields that do not follow the rule `<sig>pen`, as
/// `(peripherals, signals, field)`. The field must exist in the peripheral's
/// `route` register. A trailing `*` matches any suffix.
const LOCATION_ENABLE_EXCEPTIONS: &[(&[&str], &[&str], &str)] = &[
    (&["ACMP*"], &["OUT"], "acmppen"),
    (&["CMU"], &["CLK0"], "clkout0pen"),
    (&["CMU"], &["CLK1"], "clkout1pen"),
    (&["EBI"], &["AD*", "REn", "WEn"], "ebipen"),
    (&["EBI"], &["BL0", "BL1"], "blpen"),
    (&["EBI"], &["NANDREn", "NANDWEn"], "nandpen"),
    (&["EBI"], &["DCLK", "HSNC", "VSNC"], "tftpen"),
    (&["EBI"], &["DTEN"], "dataenpen"),
];

/// Series 0 peripherals whose pins route through `GPIO.ROUTE` (the
/// `SWLOCATION` and `ETMLOCATION` fields), not through a ROUTE register of
/// their own. The metadata does not model this, so `gen` gives them no
/// entries and reports them.
const ROUTED_BY_GPIO: &[&str] = &["DBG", "ETM"];

/// Enable fields of fixed pins. Each field must exist in the named register
/// of the peripheral's own register YAML. A fixed pin that is not in this
/// table has no enable.
///
/// | Series | Peripheral | Signals | Enable |
/// |---|---|---|---|
/// | 0 | `USB` | `DM`, `DP`, `ID` | `route.phypen` |
/// | 2 | `GPIO` | `SWCLK`, `SWDIO`, `TDO`, `TDI` | `dbgroutepen.swclktckpen`, `.swdiotmspen`, `.tdopen`, `.tdipen` |
/// | 2 | `GPIO` | `SWV`, `TRACECLK`, `TRACEDATA0..3` | `traceroutepen.swvpen`, `.traceclkpen`, `.tracedata0pen..3pen` |
const FIXED_ENABLES: &[(bool, &str, &str, &str, &str)] = &[
    (true, "USB", "DM", "route", "phypen"),
    (true, "USB", "DP", "route", "phypen"),
    (true, "USB", "ID", "route", "phypen"),
    (false, "GPIO", "SWCLK", "dbgroutepen", "swclktckpen"),
    (false, "GPIO", "SWDIO", "dbgroutepen", "swdiotmspen"),
    (false, "GPIO", "TDO", "dbgroutepen", "tdopen"),
    (false, "GPIO", "TDI", "dbgroutepen", "tdipen"),
    (false, "GPIO", "SWV", "traceroutepen", "swvpen"),
    (false, "GPIO", "TRACECLK", "traceroutepen", "traceclkpen"),
    (false, "GPIO", "TRACEDATA0", "traceroutepen", "tracedata0pen"),
    (false, "GPIO", "TRACEDATA1", "traceroutepen", "tracedata1pen"),
    (false, "GPIO", "TRACEDATA2", "traceroutepen", "tracedata2pen"),
    (false, "GPIO", "TRACEDATA3", "traceroutepen", "tracedata3pen"),
];

/// The enable field of a fixed pin, from [`FIXED_ENABLES`].
fn fixed_enable(
    regs: &RegisterNames,
    module: &str,
    series0: bool,
    peripheral: &str,
    signal: &str,
) -> Result<Option<PinRouteEnable>> {
    let Some(&(_, _, _, register, field)) = FIXED_ENABLES
        .iter()
        .find(|(s0, p, s, _, _)| *s0 == series0 && *p == peripheral && *s == signal)
    else {
        return Ok(None);
    };
    ensure!(
        regs.has_field(register, field),
        "{module} has no `{register}.{field}` for {peripheral} {signal} (from FIXED_ENABLES)"
    );
    Ok(Some(PinRouteEnable::new(register, field)))
}

/// Resolve a Series 0 location entry: the peripheral must have a `route`
/// register. When it has no `location` field, every location of the signal
/// must be 0.
fn location_route(
    regs: &RegisterNames,
    module: &str,
    peripheral: &str,
    signal: &str,
    all: &BTreeMap<u8, PinId>,
    location: u8,
) -> Result<PinRoute> {
    ensure!(regs.registers.contains_key("route"), "{module} has no `route` register");
    if !regs.has_field("route", "location") {
        ensure!(
            all.keys().all(|&n| n == 0),
            "{module} `route` has no `location` field, but {peripheral} {signal} has locations {:?}",
            all.keys().collect::<Vec<_>>()
        );
    }
    let exception = LOCATION_ENABLE_EXCEPTIONS
        .iter()
        .find(|(p, s, _)| matches_any(p, peripheral) && matches_any(s, signal));
    let field = match exception {
        Some((_, _, f)) => {
            ensure!(
                regs.has_field("route", f),
                "{module} has no `route.{f}` for {peripheral} {signal} (from the enable exception table)"
            );
            Some((*f).to_owned())
        }
        None => {
            let f = format!("{}pen", signal.to_ascii_lowercase());
            regs.has_field("route", &f).then_some(f)
        }
    };
    Ok(PinRoute::Location {
        location,
        enable: field.map(|field| PinRouteEnable {
            register: "route".to_owned(),
            field,
        }),
    })
}

/// Series 2 analog bus of each GPIO port. The pin tool calls every bus
/// `ABUS`, but the GPIO allocates port A in `ABUSALLOC`, port B in
/// `BBUSALLOC`, and ports C and D in `CDBUSALLOC`.
const ANALOG_BUSES: &[(u8, &str)] = &[(0, "ABUS"), (1, "BBUS"), (2, "CDBUS"), (3, "CDBUS")];

/// The analog bus of a Series 2 pin. The `<bus>alloc` register must exist
/// in the GPIO register YAML.
fn analog_route(gpio: &RegisterNames, gpio_module: &str, file_bus: &str, pin: PinId) -> Result<PinRoute> {
    ensure!(
        file_bus == "ABUS",
        "unknown analog bus `{file_bus}` in the pin file (only `ABUS` is known)"
    );
    let bus = ANALOG_BUSES
        .iter()
        .find(|(port, _)| *port == pin.0)
        .map(|(_, b)| *b)
        .with_context(|| format!("pin {} has no analog bus", pin_name(pin, false)))?;
    let register = format!("{}alloc", bus.to_ascii_lowercase());
    ensure!(
        gpio.registers.contains_key(&register),
        "{gpio_module} has no `{register}` register for the analog bus of {}",
        pin_name(pin, false)
    );
    Ok(PinRoute::Analog { bus: bus.to_owned() })
}

/// The chip peripheral that owns the entries of pin-file peripheral
/// `periph`: the same name, else the [`PERIPHERAL_NAMES`] entry.
pub fn chip_peripheral(owners: &BTreeMap<String, usize>, periph: &str) -> Option<usize> {
    let mapped = PERIPHERAL_NAMES
        .iter()
        .find(|(from, _)| *from == periph)
        .map(|(_, to)| *to);
    owners
        .get(periph)
        .or_else(|| mapped.and_then(|m| owners.get(m)))
        .copied()
}

/// Canonical peripheral names of a chip to the index of the instance that
/// owns the metadata entry, from [`crate::chips::canonical_names`].
pub fn owners(chip: &ChipFile) -> BTreeMap<String, usize> {
    let names: Vec<&str> = chip.peripherals.iter().map(|p| p.name.as_str()).collect();
    crate::chips::canonical_names(&names)
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| c.map(|c| (c, i)))
        .collect()
}

/// Family file of a chip: the file whose stem is the chip's lowercase
/// `family` (`efr32mg24`, `efm32gg`).
pub fn family_of<'a>(families: &'a BTreeMap<String, Family>, family: &str) -> Option<(&'a str, &'a Family)> {
    families
        .get_key_value(&family.to_ascii_lowercase())
        .map(|(s, f)| (s.as_str(), f))
}

/// Add the peripheral pin entries and the `em2` flags to a chip. On Series
/// 0, also set the bonded pins from the `packages` entry.
///
/// Returns the warnings: the chip has no pin data, or a peripheral in the
/// pin file is not on the chip.
pub fn attach(
    chip: &mut ChipFile,
    families: &BTreeMap<String, Family>,
    regs: &mut RegisterCache,
) -> Result<Vec<String>> {
    let name = chip.chip.name.clone();
    let series = chip
        .chip
        .series
        .with_context(|| format!("{name}: series missing"))?
        .series;
    let series0 = series == 0;
    let mut warnings = Vec::new();

    let Some((stem, family)) = family_of(families, &chip.chip.family) else {
        warnings.push(format!("{name}: no pin data in data/pins"));
        return Ok(warnings);
    };
    ensure!(
        family.series0() == series0,
        "{name}: data/pins/{stem}.yaml is for Series {}, the chip is Series {series}",
        if family.series0() { 0 } else { 2 }
    );

    let owner = owners(chip);

    let gpio = chip
        .peripherals
        .iter()
        .find(|p| p.kind == "gpio")
        .map(|p| (p.kind.clone(), p.register_version.clone()))
        .with_context(|| format!("{name}: no GPIO peripheral"))?;
    let gpio_module = format!("{}_{}", gpio.0, gpio.1);

    let bonded: BTreeSet<PinId> = if series0 {
        let lower = name.to_ascii_lowercase();
        let Some(package) = family.packages.iter().find(|p| p.parts.contains(&lower)) else {
            warnings.push(format!(
                "{name}: no package in data/pins/{stem}.yaml (the pin tool has no data for this part), so no peripheral pins, and Metadata.pins falls back to 16 pins for each GPIO port"
            ));
            return Ok(warnings);
        };
        let ports = regs
            .get(&gpio_module)?
            .arrays
            .get("p_ctrl")
            .copied()
            .with_context(|| format!("{gpio_module}: no p_ctrl register array"))?;
        for &(port, pin) in &package.pins {
            ensure!(
                u32::from(port) < ports && pin < 16,
                "{name}: package pin {} is outside the {ports} GPIO ports of 16 pins",
                pin_name((port, pin), true)
            );
        }
        // Series 0 device headers have no pin masks. The package gives the
        // bonded pins.
        ensure!(chip.pins.is_empty(), "{name}: Series 0 chip already has pins");
        chip.pins = package
            .pins
            .iter()
            .map(|&(port, pin)| crate::chips::Pin { port, pin, em2: None })
            .collect();
        package.pins.clone()
    } else {
        chip.pins.iter().map(|p| (p.port, p.pin)).collect()
    };

    let mut skipped = BTreeSet::new();
    let mut by_gpio = BTreeSet::new();
    let mut entries: BTreeMap<usize, BTreeMap<(String, PinRoute), BTreeSet<PinId>>> = BTreeMap::new();
    for (periph, signals) in &family.peripherals {
        if series0 && ROUTED_BY_GPIO.contains(&periph.as_str()) {
            by_gpio.insert(periph.as_str());
            continue;
        }
        let Some(index) = chip_peripheral(&owner, periph) else {
            skipped.insert(periph.as_str());
            continue;
        };
        let p = &chip.peripherals[index];
        let module = format!("{}_{}", p.kind, p.register_version);
        for (sig, signal) in signals {
            let ctx = || format!("{name}: {periph} {sig}");
            if !series0
                && matches!(signal, Signal::Pins(_) | Signal::Alternatives(_))
                && let Some(reg) = route_register(regs.get(&gpio_module)?, &gpio.1, periph, sig)
            {
                anyhow::bail!(
                    "{}: the pin file gives fixed pins, but {gpio_module} has the route register `{reg}`. Use `ports`.",
                    ctx()
                );
            }
            for (number, pin) in signal.filter(&bonded) {
                let route = match signal {
                    Signal::Ports(_) => {
                        ensure!(!series0, "{}: `ports` on a Series 0 chip", ctx());
                        dbus_route(regs.get(&gpio_module)?, &gpio.1, periph, sig).with_context(ctx)?
                    }
                    Signal::Locations(all) => {
                        location_route(regs.get(&module)?, &module, periph, sig, all, number).with_context(ctx)?
                    }
                    Signal::Bus(bus) => {
                        ensure!(!series0, "{}: `bus` on a Series 0 chip", ctx());
                        analog_route(regs.get(&gpio_module)?, &gpio_module, bus, pin).with_context(ctx)?
                    }
                    Signal::Pins(_) | Signal::Alternatives(_) => PinRoute::Fixed {
                        enable: fixed_enable(regs.get(&module)?, &module, series0, periph, sig).with_context(ctx)?,
                        alternative: matches!(signal, Signal::Alternatives(_)).then_some(number),
                    },
                };
                check_enable(series0, periph, sig, &route).with_context(ctx)?;
                entries
                    .entry(index)
                    .or_default()
                    .entry((sig.clone(), route))
                    .or_default()
                    .insert(pin);
            }
        }
    }
    if !skipped.is_empty() {
        warnings.push(format!("{name}: pin peripherals with no peripheral: {skipped:?}"));
    }
    if !by_gpio.is_empty() {
        warnings.push(format!(
            "{name}: pin peripherals routed by GPIO.ROUTE, not modeled: {by_gpio:?}"
        ));
    }

    for (index, groups) in entries {
        chip.peripherals[index].pins = groups
            .into_iter()
            .map(|((signal, route), pins)| PeripheralPins {
                signal,
                route,
                pins: pins.into_iter().map(|p| pin_name(p, series0)).collect(),
            })
            .collect();
    }

    if let Some(em2) = &family.em2 {
        for pin in &mut chip.pins {
            pin.em2 = Some(em2.contains((pin.port, pin.pin)));
        }
    }
    Ok(warnings)
}

/// Bonded pins of a Series 2 chip, as [`Pin`] rows, for tests.
#[cfg(test)]
fn pins_of(ids: &[PinId]) -> Vec<Pin> {
    ids.iter().map(|&(port, pin)| Pin { port, pin, em2: None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_names_follow_the_series_format() {
        assert_eq!(pin_name((0, 5), false), "PA05");
        assert_eq!(pin_name((4, 10), true), "PE10");
        assert_eq!(parse_pin("PA05", false).unwrap(), (0, 5));
        assert_eq!(parse_pin("PA5", true).unwrap(), (0, 5));
        assert!(parse_pin("PA5", false).is_err());
        assert!(parse_pin("PA05", true).is_err());
    }

    #[test]
    fn yaml_round_trips() {
        let mut f = Family {
            em2: Some(Em2 {
                ports: [0].into(),
                pins: [(2, 1)].into(),
            }),
            ..Default::default()
        };
        f.peripherals.entry("UART9".into()).or_default().extend([
            ("TX".to_owned(), Signal::Ports([0, 1].into())),
            ("N".to_owned(), Signal::Pins([(3, 2)].into())),
            ("POS".to_owned(), Signal::Bus("ABUS".into())),
        ]);
        let text = f.to_yaml(&["test".into()]);
        assert!(text.contains("    TX: { ports: [A, B] }\n"), "{text}");
        assert!(text.contains("    \"N\": { pins: [PD02] }\n"), "{text}");
        assert_eq!(Family::parse(&text).unwrap(), f);

        let mut s0 = Family {
            packages: vec![Package {
                parts: vec!["x1".into()],
                pins: [(0, 0), (0, 1), (1, 3)].into(),
            }],
            ..Default::default()
        };
        s0.peripherals
            .entry("UART9".into())
            .or_default()
            .insert("TX".into(), Signal::Locations([(0, (0, 1)), (2, (1, 3))].into()));
        let text = s0.to_yaml(&[]);
        assert!(text.contains("    pins: [PA0, PA1,\n      PB3]\n"), "{text}");
        assert!(text.contains("    TX: { locations: { 0: PA1, 2: PB3 } }\n"), "{text}");
        assert_eq!(Family::parse(&text).unwrap(), s0);
    }

    #[test]
    fn dbus_route_uses_rule_and_exceptions() {
        let gpio = RegisterNames::parse(
            "block/Gpio:\n  items:\n  - name: uart9_txroute\n    byte_offset: 0\n    fieldset: regs::R\n  - name: uart9_ctsroute\n    byte_offset: 4\n    fieldset: regs::R\n  - name: uart9_routeen\n    byte_offset: 8\n    fieldset: regs::En\n  - name: timer0_cdti0route\n    byte_offset: 12\n    fieldset: regs::R\n  - name: timer0_routeen\n    byte_offset: 16\n    fieldset: regs::Ten\nfieldset/regs::R:\n  fields:\n  - name: port\n    bit_offset: 0\n    bit_size: 2\nfieldset/regs::En:\n  fields:\n  - name: txpen\n    bit_offset: 0\n    bit_size: 1\nfieldset/regs::Ten:\n  fields:\n  - name: ccc0pen\n    bit_offset: 3\n    bit_size: 1\n",
        )
        .unwrap();
        let en = |r: &str, f: &str| {
            Some(PinRouteEnable {
                register: r.into(),
                field: f.into(),
            })
        };
        assert_eq!(
            dbus_route(&gpio, "s2v9", "UART9", "TX").unwrap(),
            PinRoute::Dbus {
                register: "uart9_txroute".into(),
                enable: en("uart9_routeen", "txpen")
            }
        );
        assert_eq!(
            dbus_route(&gpio, "s2v9", "UART9", "CTS").unwrap(),
            PinRoute::Dbus {
                register: "uart9_ctsroute".into(),
                enable: None
            }
        );
        assert_eq!(
            dbus_route(&gpio, "s2v9", "TIMER0", "CDTI0").unwrap(),
            PinRoute::Dbus {
                register: "timer0_cdti0route".into(),
                enable: en("timer0_routeen", "ccc0pen")
            }
        );
        let err = dbus_route(&gpio, "s2v9", "UART9", "RX").unwrap_err().to_string();
        assert!(err.contains("no route register `uart9_rxroute`"), "{err}");
    }

    #[test]
    fn filter_keeps_bonded_pins_only() {
        let bonded: BTreeSet<PinId> = pins_of(&[(0, 0), (0, 3), (2, 1)])
            .iter()
            .map(|p| (p.port, p.pin))
            .collect();
        assert_eq!(
            Signal::Ports([0].into()).filter(&bonded),
            [(0, (0, 0)), (0, (0, 3))].into()
        );
        assert_eq!(
            Signal::Pins([(2, 1), (2, 2)].into()).filter(&bonded),
            [(0, (2, 1))].into()
        );
        assert_eq!(Signal::Bus("ABUS".into()).filter(&bonded).len(), 3);
        assert_eq!(
            Signal::Locations([(0, (0, 3)), (1, (1, 1))].into()).filter(&bonded),
            [(0, (0, 3))].into()
        );
    }
    /// Register names parsed from a one-block YAML of `(register, fields)`.
    fn regs_yaml(registers: &[(&str, &[&str])]) -> RegisterNames {
        let mut s = String::from("block/B:\n  items:\n");
        for (i, (r, _)) in registers.iter().enumerate() {
            s += &format!("  - name: {r}\n    byte_offset: {}\n    fieldset: regs::F{i}\n", i * 4);
        }
        for (i, (_, fields)) in registers.iter().enumerate() {
            s += &format!("fieldset/regs::F{i}:\n  fields:\n");
            for (b, f) in fields.iter().enumerate() {
                s += &format!("  - name: {f}\n    bit_offset: {b}\n    bit_size: 1\n");
            }
        }
        RegisterNames::parse(&s).unwrap()
    }

    fn instance(name: &str, kind: &str, version: &str) -> crate::chips::PeripheralInstance {
        crate::chips::PeripheralInstance {
            name: name.into(),
            base_address: 0,
            version: None,
            kind: kind.into(),
            register_version: version.into(),
            block: kind.to_ascii_uppercase(),
            interrupts: vec![],
            dma_requests: vec![],
            pins: vec![],
        }
    }

    fn chip(series: u8, family: &str, pins: &[PinId], peripherals: Vec<crate::chips::PeripheralInstance>) -> ChipFile {
        ChipFile {
            chip: crate::pdsc::Chip {
                name: "XCHIP9".into(),
                family: family.into(),
                core: "CM33".into(),
                fpu: false,
                mpu: false,
                trustzone: false,
                series: Some(crate::header::Series { series, config: 0 }),
                nvic_prio_bits: None,
                flash_page_size: None,
                memory: vec![],
                flash_algo: None,
                svd: "x.svd".into(),
                package: None,
            },
            peripherals,
            interrupts: vec![],
            pins: pins_of(pins),
            dma_channel_count: 0,
        }
    }

    const S2_GPIO: &[(&str, &[&str])] = &[
        ("usart9_txroute", &["port", "pin"]),
        ("usart9_ctsroute", &["port", "pin"]),
        ("usart9_rtsroute", &["port", "pin"]),
        ("usart9_routeen", &["txpen"]),
        ("abusalloc", &["aeven0"]),
        ("bbusalloc", &["beven0"]),
        ("cdbusalloc", &["cdeven0"]),
    ];

    /// Attach `yaml` to a made-up Series 2 chip with the GPIO IR `gpio`.
    fn attach_s2(yaml: &str, gpio: &[(&str, &[&str])]) -> Result<ChipFile> {
        let families: BTreeMap<String, Family> = [("xfam9".to_owned(), Family::parse(yaml).unwrap())].into();
        let dir = Path::new("/nonexistent");
        let mut regs = RegisterCache::new(dir);
        regs.insert("gpio_s2v9", regs_yaml(gpio));
        regs.insert("usart_s2v9", regs_yaml(&[]));
        regs.insert("adc_s2v9", regs_yaml(&[]));
        let mut c = chip(
            2,
            "XFAM9",
            &[(0, 0), (0, 2), (0, 10), (1, 0), (2, 0), (3, 3)],
            vec![
                instance("GPIO", "gpio", "s2v9"),
                instance("USART9", "usart", "s2v9"),
                instance("ADC9", "adc", "s2v9"),
            ],
        );
        attach(&mut c, &families, &mut regs)?;
        Ok(c)
    }

    #[test]
    fn attach_groups_pins_by_signal_and_route() {
        let c = attach_s2(
            "peripherals:\n  USART9:\n    TX: { ports: [A, B] }\n    CTS: { ports: [A] }\n",
            S2_GPIO,
        )
        .unwrap();
        let usart = &c.peripherals[1].pins;
        // One entry for each (signal, route), sorted by signal. The pins are
        // sorted by port, then by pin number.
        assert_eq!(
            usart,
            &vec![
                PeripheralPins {
                    signal: "CTS".into(),
                    route: PinRoute::Dbus {
                        register: "usart9_ctsroute".into(),
                        enable: None
                    },
                    pins: vec!["PA00".into(), "PA02".into(), "PA10".into()],
                },
                PeripheralPins {
                    signal: "TX".into(),
                    route: PinRoute::Dbus {
                        register: "usart9_txroute".into(),
                        enable: Some(PinRouteEnable::new("usart9_routeen", "txpen"))
                    },
                    pins: vec!["PA00".into(), "PA02".into(), "PA10".into(), "PB00".into()],
                },
            ]
        );
    }

    #[test]
    fn analog_bus_follows_the_port() {
        let c = attach_s2("peripherals:\n  ADC9:\n    IN: { bus: ABUS }\n", S2_GPIO).unwrap();
        let got: Vec<(String, Vec<String>)> = c.peripherals[2]
            .pins
            .iter()
            .map(|e| match &e.route {
                PinRoute::Analog { bus } => (bus.clone(), e.pins.clone()),
                r => panic!("not analog: {r:?}"),
            })
            .collect();
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            got,
            vec![
                ("ABUS".into(), v(&["PA00", "PA02", "PA10"])),
                ("BBUS".into(), v(&["PB00"])),
                ("CDBUS".into(), v(&["PC00", "PD03"])),
            ]
        );
        // The `<bus>alloc` register must exist.
        let no_bbus: Vec<(&str, &[&str])> = S2_GPIO.iter().copied().filter(|r| r.0 != "bbusalloc").collect();
        let err = format!(
            "{:#}",
            attach_s2("peripherals:\n  ADC9:\n    IN: { bus: ABUS }\n", &no_bbus)
                .err()
                .unwrap()
        );
        assert!(
            err.contains("gpio_s2v9 has no `bbusalloc` register for the analog bus of PB00"),
            "{err}"
        );
    }

    #[test]
    fn missing_enable_needs_an_allowlist_entry() {
        // RTS has a route register but no `rtspen`, and NO_ENABLE has no
        // entry for it.
        let err = format!(
            "{:#}",
            attach_s2("peripherals:\n  USART9:\n    RTS: { ports: [A] }\n", S2_GPIO)
                .err()
                .unwrap()
        );
        assert!(
            err.contains(
                "XCHIP9: USART9 RTS: USART9 RTS: the route has no enable field, and NO_ENABLE has no entry for it"
            ),
            "{err}"
        );
        // A stale entry is also an error: CTS is input-only in NO_ENABLE.
        let mut gpio: Vec<(&str, &[&str])> = S2_GPIO.to_vec();
        gpio[3] = ("usart9_routeen", &["txpen", "ctspen"]);
        let err = format!(
            "{:#}",
            attach_s2("peripherals:\n  USART9:\n    CTS: { ports: [A] }\n", &gpio)
                .err()
                .unwrap()
        );
        assert!(
            err.contains("NO_ENABLE says \"input-only\", but the route has the enable `usart9_routeen.ctspen`"),
            "{err}"
        );
    }

    #[test]
    fn fixed_pins_with_a_route_register_are_rejected() {
        let err = format!(
            "{:#}",
            attach_s2("peripherals:\n  USART9:\n    TX: { pins: [PA00] }\n", S2_GPIO)
                .err()
                .unwrap()
        );
        assert!(
            err.contains("XCHIP9: USART9 TX: the pin file gives fixed pins, but gpio_s2v9 has the route register `usart9_txroute`"),
            "{err}"
        );
    }

    #[test]
    fn series0_uses_package_pins_and_numbered_entries() {
        let yaml = "packages:\n  - parts: [xchip9]\n    pins: [PA2, PA10, PB1, PC4]\nperipherals:\n  USART9:\n    TX: { locations: { 0: PA10, 1: PA2, 2: PA10, 3: PC5 } }\n  DAC9:\n    OUT: { pins: [PA10, PA2] }\n    OUTALT: { alternatives: { 0: PB1, 4: PC4 } }\n  USB:\n    DP: { pins: [PB1] }\n";
        let families: BTreeMap<String, Family> = [("xfam9".to_owned(), Family::parse(yaml).unwrap())].into();
        let mut regs = RegisterCache::new(Path::new("/nonexistent"));
        let mut gpio = regs_yaml(&[]);
        gpio.arrays.insert("p_ctrl".into(), 6);
        regs.insert("gpio_s0v9", gpio);
        regs.insert("usart_s0v9", regs_yaml(&[("route", &["txpen", "location"])]));
        regs.insert("dac_s0v9", regs_yaml(&[]));
        regs.insert("usb_s0v9", regs_yaml(&[("route", &["phypen"])]));
        let mut c = chip(
            0,
            "XFAM9",
            &[],
            vec![
                instance("GPIO", "gpio", "s0v9"),
                instance("USART9", "usart", "s0v9"),
                instance("DAC9", "dac", "s0v9"),
                instance("USB", "usb", "s0v9"),
            ],
        );
        attach(&mut c, &families, &mut regs).unwrap();
        // Metadata.pins comes from the package.
        assert_eq!(c.pins, pins_of(&[(0, 2), (0, 10), (1, 1), (2, 4)]));
        let tx = PinRouteEnable::new("route", "txpen");
        let loc = |n| PinRoute::Location {
            location: n,
            enable: Some(tx.clone()),
        };
        // PA10 is at locations 0 and 2. PC5 is not bonded, so there is no 3.
        let got: Vec<(PinRoute, Vec<String>)> = c.peripherals[1]
            .pins
            .iter()
            .map(|e| (e.route.clone(), e.pins.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (loc(0), vec!["PA10".to_owned()]),
                (loc(1), vec!["PA2".to_owned()]),
                (loc(2), vec!["PA10".to_owned()]),
            ]
        );
        let dac = &c.peripherals[2].pins;
        assert_eq!(dac[0].signal, "OUT");
        assert_eq!(
            dac[0].route,
            PinRoute::Fixed {
                enable: None,
                alternative: None
            }
        );
        // Sorted by port, then by pin number: PA2 before PA10.
        assert_eq!(dac[0].pins, vec!["PA2".to_owned(), "PA10".to_owned()]);
        let alts: Vec<(PinRoute, Vec<String>)> = dac[1..].iter().map(|e| (e.route.clone(), e.pins.clone())).collect();
        assert_eq!(
            alts,
            vec![
                (
                    PinRoute::Fixed {
                        enable: None,
                        alternative: Some(0)
                    },
                    vec!["PB1".to_owned()]
                ),
                (
                    PinRoute::Fixed {
                        enable: None,
                        alternative: Some(4)
                    },
                    vec!["PC4".to_owned()]
                ),
            ]
        );
        // A fixed pin with an enable field in FIXED_ENABLES.
        assert_eq!(
            c.peripherals[3].pins[0].route,
            PinRoute::Fixed {
                enable: Some(PinRouteEnable::new("route", "phypen")),
                alternative: None
            }
        );
    }
}
