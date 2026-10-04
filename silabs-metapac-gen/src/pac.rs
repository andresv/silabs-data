//! Render per-IP PAC modules to `src/peripherals/<kind>_<version>.rs`.
//!
//! Each file holds the chiptool PAC for one `(kind, version)` pair: the
//! peripheral struct (`pub struct Timer { ptr }`), its accessors, and the
//! `regs`/`vals` submodules.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use chiptool::generate::{self, CommonModule, DefmtOption, Options};
use chiptool::ir::IR;

/// `(kind, version)` key for the peripheral module set.
pub type IpKey = (String, String);

pub fn module_name(kind: &str, version: &str) -> String {
    format!("{kind}_{version}")
}

/// Convenience wrapper accepting a `(kind, version)` tuple.
pub fn module_name_from_key(key: &IpKey) -> String {
    module_name(&key.0, &key.1)
}

/// Render one Rust file per IR under `<out_dir>/<module_name>.rs`.
/// Each file is rustfmt'd in place before returning.
pub fn write_peripherals_dir(irs: &BTreeMap<IpKey, IR>, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    remove_stale_rs_files(irs, out_dir)?;

    let opts = Options::default()
        .with_common_module(CommonModule::External("crate::common".parse().expect("static path")))
        .with_defmt(DefmtOption::Feature("defmt".to_owned()))
        .with_skip_no_std(true);

    for (key, ir) in irs {
        let mod_name = module_name_from_key(key);
        let tokens = generate::render(ir, &opts).with_context(|| format!("render {mod_name}"))?;
        let body = strip_inner_attrs_and_doc(&tokens.to_string());
        let path = out_dir.join(format!("{mod_name}.rs"));
        std::fs::write(&path, &body).with_context(|| format!("write {}", path.display()))?;
        rustfmt_in_place(&path).with_context(|| format!("rustfmt {}", path.display()))?;
    }
    Ok(())
}

/// Remove `.rs` files in `out_dir` that have no IR in `irs`.
///
/// The output directory is reused, so a dropped module would otherwise stay.
pub(crate) fn remove_stale_rs_files(irs: &BTreeMap<IpKey, IR>, out_dir: &Path) -> Result<()> {
    let expected: BTreeSet<String> = irs
        .keys()
        .map(|key| format!("{}.rs", module_name_from_key(key)))
        .collect();
    for entry in std::fs::read_dir(out_dir).with_context(|| format!("read {}", out_dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && !expected.contains(file_name)
        {
            std::fs::remove_file(&path).with_context(|| format!("remove stale generated file {}", path.display()))?;
        }
    }
    Ok(())
}

/// Invoke `rustfmt` to reformat `path` in place.
///
/// `TokenStream::to_string()` has no line breaks or indentation. This calls
/// `rustfmt` directly, not `cargo fmt`, because the files live outside the
/// workspace.
pub(crate) fn rustfmt_in_place(path: &Path) -> Result<()> {
    let status = std::process::Command::new("rustfmt")
        .arg("--edition=2024")
        .arg("--emit=files")
        .arg(path)
        .status()
        .with_context(|| "spawn rustfmt — ensure it's on PATH (e.g. `rustup component add rustfmt`)")?;
    if !status.success() {
        anyhow::bail!("rustfmt failed on {}: {status}", path.display());
    }
    Ok(())
}

/// Strip leading inner attributes and inner doc comments emitted by chiptool.
///
/// The parent file already sets `#![no_std]` and the `#![allow(...)]` lints.
/// An inner attribute here would duplicate them or trigger E0753.
fn strip_inner_attrs_and_doc(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    loop {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        let start = i;
        if bytes[i] != b'#' {
            break;
        }
        i += 1;
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || bytes[i] != b'!' {
            i = start;
            break;
        }
        i += 1;
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || bytes[i] != b'[' {
            i = start;
            break;
        }
        let mut depth = 1usize;
        i += 1;
        while i < len && depth > 0 {
            match bytes[i] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
    }
    s[i..].to_owned()
}

/// Generate the chiptool common module, written once at lib.rs's root.
pub fn write_common_module(out_path: &Path) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, generate::COMMON_MODULE)
        .with_context(|| format!("write common module {}", out_path.display()))?;
    Ok(())
}
