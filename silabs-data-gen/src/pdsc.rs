use anyhow::{Context, Result, anyhow, bail};
use quick_xml::events::Event;
use quick_xml::events::attributes::Attributes;
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChipDb {
    pub family: String,
    pub chips: Vec<Chip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    pub name: String,
    pub core: String,
    pub fpu: bool,
    pub mpu: bool,
    pub trustzone: bool,
    pub memory: Vec<MemoryRegion>,
    pub flash_algo: Option<String>,
    pub svd: String,
    pub package: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRegion {
    pub id: String,
    pub start: u64,
    pub size: u64,
    pub access: String,
}

/// In-progress processor info inherited down the family/subFamily/device chain.
#[derive(Debug, Default, Clone)]
struct ProcInfo {
    core: Option<String>,
    fpu: Option<bool>,
    mpu: Option<bool>,
    trustzone: Option<bool>,
}

impl ProcInfo {
    fn merged_with(&self, other: &ProcInfo) -> ProcInfo {
        ProcInfo {
            core: other.core.clone().or_else(|| self.core.clone()),
            fpu: other.fpu.or(self.fpu),
            mpu: other.mpu.or(self.mpu),
            trustzone: other.trustzone.or(self.trustzone),
        }
    }
}

/// One level of inherited context (family or subFamily scope).
#[derive(Debug, Default, Clone)]
struct Scope {
    proc_info: ProcInfo,
    memory: Vec<MemoryRegion>,
    flash_algo: Option<String>,
    svd: Option<String>,
}

pub fn parse(xml: &str) -> Result<ChipDb> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();

    let mut family: Option<String> = None;
    let mut chips: Vec<Chip> = Vec::new();

    // Stack of scopes: index 0 = family scope, index 1 = subFamily scope (when present).
    let mut scopes: Vec<Scope> = Vec::new();
    // Per-device accumulator (None outside a <device>).
    let mut current_device: Option<DeviceBuilder> = None;
    let mut in_devices = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                let local = std::str::from_utf8(name.as_ref())?.to_owned();
                match local.as_str() {
                    "devices" => in_devices = true,
                    "family" if in_devices => {
                        let dfam =
                            attr(e.attributes(), "Dfamily")?.ok_or_else(|| anyhow!("<family> missing Dfamily"))?;
                        family = Some(family_short_name(&dfam));
                        scopes.push(Scope::default());
                    }
                    "subFamily" if in_devices => {
                        scopes.push(Scope::default());
                    }
                    "device" if in_devices => {
                        let dname = attr(e.attributes(), "Dname")?.ok_or_else(|| anyhow!("<device> missing Dname"))?;
                        current_device = Some(DeviceBuilder::new(dname));
                    }
                    "processor" if in_devices => {
                        let info = parse_processor(e.attributes())?;
                        if let Some(dev) = current_device.as_mut() {
                            dev.proc_info = dev.proc_info.merged_with(&info);
                        } else if let Some(scope) = scopes.last_mut() {
                            scope.proc_info = scope.proc_info.merged_with(&info);
                        }
                    }
                    "memory" if in_devices => {
                        let mem = parse_memory(e.attributes())?;
                        if let Some(dev) = current_device.as_mut() {
                            dev.memory.push(mem);
                        } else if let Some(scope) = scopes.last_mut() {
                            scope.memory.push(mem);
                        }
                    }
                    "algorithm" if in_devices => {
                        let algo = attr(e.attributes(), "name")?;
                        if let Some(dev) = current_device.as_mut() {
                            if dev.flash_algo.is_none() {
                                dev.flash_algo = algo;
                            }
                        } else if let Some(scope) = scopes.last_mut()
                            && scope.flash_algo.is_none()
                        {
                            scope.flash_algo = algo;
                        }
                    }
                    "debug" if in_devices => {
                        let svd = attr(e.attributes(), "svd")?;
                        if let Some(dev) = current_device.as_mut() {
                            if dev.svd.is_none() {
                                dev.svd = svd;
                            }
                        } else if let Some(scope) = scopes.last_mut()
                            && scope.svd.is_none()
                        {
                            scope.svd = svd;
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                let local = std::str::from_utf8(name.as_ref())?.to_owned();
                // Empty (self-closing) elements use the same handling as Start
                // for the leaf tags we care about.
                match local.as_str() {
                    "processor" if in_devices => {
                        let info = parse_processor(e.attributes())?;
                        if let Some(dev) = current_device.as_mut() {
                            dev.proc_info = dev.proc_info.merged_with(&info);
                        } else if let Some(scope) = scopes.last_mut() {
                            scope.proc_info = scope.proc_info.merged_with(&info);
                        }
                    }
                    "memory" if in_devices => {
                        let mem = parse_memory(e.attributes())?;
                        if let Some(dev) = current_device.as_mut() {
                            dev.memory.push(mem);
                        } else if let Some(scope) = scopes.last_mut() {
                            scope.memory.push(mem);
                        }
                    }
                    "algorithm" if in_devices => {
                        let algo = attr(e.attributes(), "name")?;
                        if let Some(dev) = current_device.as_mut() {
                            if dev.flash_algo.is_none() {
                                dev.flash_algo = algo;
                            }
                        } else if let Some(scope) = scopes.last_mut()
                            && scope.flash_algo.is_none()
                        {
                            scope.flash_algo = algo;
                        }
                    }
                    "debug" if in_devices => {
                        let svd = attr(e.attributes(), "svd")?;
                        if let Some(dev) = current_device.as_mut() {
                            if dev.svd.is_none() {
                                dev.svd = svd;
                            }
                        } else if let Some(scope) = scopes.last_mut()
                            && scope.svd.is_none()
                        {
                            scope.svd = svd;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.name();
                let local = std::str::from_utf8(name.as_ref())?.to_owned();
                match local.as_str() {
                    "devices" => in_devices = false,
                    "family" => {
                        scopes.pop();
                    }
                    "subFamily" => {
                        scopes.pop();
                    }
                    "device" => {
                        if let Some(dev) = current_device.take() {
                            chips.push(dev.finalize(&scopes)?);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let family = family.ok_or_else(|| anyhow!("no <family> element found"))?;
    Ok(ChipDb { family, chips })
}

#[derive(Debug)]
struct DeviceBuilder {
    name: String,
    proc_info: ProcInfo,
    memory: Vec<MemoryRegion>,
    flash_algo: Option<String>,
    svd: Option<String>,
}

impl DeviceBuilder {
    fn new(name: String) -> Self {
        Self {
            name,
            proc_info: ProcInfo::default(),
            memory: Vec::new(),
            flash_algo: None,
            svd: None,
        }
    }

    /// Merge this device's data with the surrounding family/subFamily scopes.
    fn finalize(self, scopes: &[Scope]) -> Result<Chip> {
        // Merge processor info: outer scopes first, then the device's own overrides.
        let mut proc_info = ProcInfo::default();
        for scope in scopes {
            proc_info = proc_info.merged_with(&scope.proc_info);
        }
        proc_info = proc_info.merged_with(&self.proc_info);

        // Memory is additive; device-level entries with the same id override
        // earlier (outer-scope) ones.
        let mut memory: Vec<MemoryRegion> = Vec::new();
        for scope in scopes {
            for m in &scope.memory {
                memory.push(m.clone());
            }
        }
        for m in self.memory {
            // Override-by-id semantics.
            if let Some(idx) = memory.iter().position(|x| x.id == m.id) {
                memory[idx] = m;
            } else {
                memory.push(m);
            }
        }

        // Flash algo / svd: device wins, else nearest enclosing scope wins.
        let flash_algo = self
            .flash_algo
            .or_else(|| scopes.iter().rev().find_map(|s| s.flash_algo.clone()));
        let svd = self
            .svd
            .or_else(|| scopes.iter().rev().find_map(|s| s.svd.clone()))
            .ok_or_else(|| anyhow!("device {} has no <debug svd=...>", self.name))?;

        let core = proc_info
            .core
            .ok_or_else(|| anyhow!("device {} has no Dcore", self.name))?;

        let package = derive_package(&self.name);

        Ok(Chip {
            name: self.name,
            core,
            fpu: proc_info.fpu.unwrap_or(false),
            mpu: proc_info.mpu.unwrap_or(false),
            trustzone: proc_info.trustzone.unwrap_or(false),
            memory,
            flash_algo,
            svd,
            package,
        })
    }
}

fn family_short_name(dfamily: &str) -> String {
    // "EFR32MG26 Series 2 Family" -> "EFR32MG26"
    dfamily.split_whitespace().next().unwrap_or(dfamily).to_owned()
}

fn parse_processor(attrs: Attributes) -> Result<ProcInfo> {
    let mut info = ProcInfo::default();
    for a in attrs {
        let a = a?;
        let key = std::str::from_utf8(a.key.as_ref())?;
        let val = a.unescape_value()?.into_owned();
        match key {
            "Dcore" => info.core = Some(val),
            "Dfpu" => info.fpu = Some(!val.eq_ignore_ascii_case("NO_FPU") && !val.is_empty()),
            "Dmpu" => info.mpu = Some(!val.eq_ignore_ascii_case("NO_MPU") && !val.is_empty()),
            "Dtz" => info.trustzone = Some(val.eq_ignore_ascii_case("TZ")),
            _ => {}
        }
    }
    Ok(info)
}

fn parse_memory(attrs: Attributes) -> Result<MemoryRegion> {
    let mut id: Option<String> = None;
    let mut start: Option<u64> = None;
    let mut size: Option<u64> = None;
    let mut explicit_access: Option<String> = None;
    for a in attrs {
        let a = a?;
        let key = std::str::from_utf8(a.key.as_ref())?;
        let val = a.unescape_value()?.into_owned();
        match key {
            "id" | "name" => id = Some(val),
            "start" => start = Some(parse_hex_or_dec(&val).context("memory start")?),
            "size" => size = Some(parse_hex_or_dec(&val).context("memory size")?),
            "access" => explicit_access = Some(val),
            _ => {}
        }
    }
    let id = id.ok_or_else(|| anyhow!("<memory> missing id/name"))?;
    let start = start.ok_or_else(|| anyhow!("<memory id={id}> missing start"))?;
    let size = size.ok_or_else(|| anyhow!("<memory id={id}> missing size"))?;
    let access = explicit_access.unwrap_or_else(|| default_access(&id));
    Ok(MemoryRegion {
        id,
        start,
        size,
        access,
    })
}

fn default_access(id: &str) -> String {
    let upper = id.to_ascii_uppercase();
    if upper.starts_with("IROM") || upper.starts_with("ROM") {
        "rx".to_string()
    } else if upper.starts_with("IRAM") || upper.starts_with("RAM") {
        "rwx".to_string()
    } else {
        "rw".to_string()
    }
}

fn parse_hex_or_dec(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    if let Some(stripped) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(stripped, 16).with_context(|| format!("invalid hex literal {trimmed:?}"))
    } else if trimmed.is_empty() {
        bail!("empty numeric literal");
    } else {
        trimmed
            .parse::<u64>()
            .with_context(|| format!("invalid decimal literal {trimmed:?}"))
    }
}

/// Best-effort: derive a package suffix like "IM68" from the device name.
fn derive_package(name: &str) -> Option<String> {
    // EFR32MG26B211F2048IM68 -> the trailing alpha-num group after the last digit
    // run that follows a 'F' is the package code. Cheap heuristic: take the
    // suffix that starts with the last 'I' in the name (matches IM68/IL136/etc).
    let bytes = name.as_bytes();
    let last_i = bytes.iter().rposition(|&b| b == b'I')?;
    let suffix = &name[last_i..];
    // Sanity: should contain at least one digit and be reasonably short.
    if suffix.len() <= 6 && suffix.chars().any(|c| c.is_ascii_digit()) {
        Some(suffix.to_owned())
    } else {
        None
    }
}

fn attr(attrs: Attributes, key: &str) -> Result<Option<String>> {
    for a in attrs {
        let a = a?;
        if std::str::from_utf8(a.key.as_ref())? == key {
            return Ok(Some(a.unescape_value()?.into_owned()));
        }
    }
    Ok(None)
}
