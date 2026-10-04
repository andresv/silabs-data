//! End-to-end SVD-to-Rust codegen for one chip's SVD via chiptool.
//!
//! Pipeline:
//! 1. Read SVD XML from disk.
//! 2. Strip `_S` TrustZone-alias peripherals ([`crate::peripheral`]).
//! 3. Parse with `svd-parser` (the embassy-rs fork).
//! 4. Convert to chiptool IR.
//! 5. Apply transforms loaded from one or more YAML files.
//! 6. Render `lib.rs` token stream + `device.x` linker fragment.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use chiptool::generate::{self, CommonModule, DefmtOption, Options};
use chiptool::ir::IR;
use chiptool::svd2ir::{self, NamespaceMode};
use chiptool::transform::Transform;
use svd_parser::ValidateLevel;

pub struct GenerateInput<'a> {
    pub svd_path: &'a Path,
    /// One or more transforms YAML files. Applied in order.
    pub transforms: &'a [&'a Path],
    /// Interrupt table for the chip, from the CMSIS device header
    /// (`silabs-data-gen/src/header.rs`). It replaces the SVD `<interrupt>`
    /// blocks.
    pub interrupts: &'a [Interrupt<'a>],
}

/// One interrupt entry for the chiptool IR.
///
/// A separate type from the chip-JSON `Interrupt`, so this crate does not
/// depend on the JSON type layout.
#[derive(Debug, Clone, Copy)]
pub struct Interrupt<'a> {
    pub name: &'a str,
    pub value: u32,
    pub description: Option<&'a str>,
}

pub struct Generated {
    pub lib_rs: String,
    pub device_x: String,
}

/// Mirror of chiptool's private `Config` struct used by its YAML loader.
#[derive(Default, serde::Deserialize)]
struct TransformConfig {
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    transforms: Vec<Transform>,
}

fn apply_transform_file(ir: &mut IR, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read transforms file {}", path.display()))?;
    let cfg: TransformConfig =
        serde_yaml::from_slice(&bytes).with_context(|| format!("parse transforms file {}", path.display()))?;
    // Resolve relative includes vs the parent directory of `path`.
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for inc in &cfg.includes {
        let sub = parent.join(inc);
        apply_transform_file(ir, &sub)?;
    }
    for t in &cfg.transforms {
        t.run(ir)
            .with_context(|| format!("apply transform from {}", path.display()))?;
    }
    Ok(())
}

pub fn generate(input: GenerateInput<'_>) -> Result<Generated> {
    let raw =
        std::fs::read_to_string(input.svd_path).with_context(|| format!("read SVD {}", input.svd_path.display()))?;

    let preprocessed = crate::peripheral::strip_secure_peripherals(&raw)?;

    let cfg = svd_parser::Config::default()
        .expand_properties(true)
        .validate_level(ValidateLevel::Disabled);
    let device = svd_parser::parse_with_config(&preprocessed, &cfg)
        .with_context(|| format!("parse SVD {}", input.svd_path.display()))?;

    // `BlockWithRegsVals` puts fieldsets in `regs::` and enums in `vals::`.
    // This prevents a clash when a register and an enum share a name (WDOG `LOCK`).
    let mut ir = svd2ir::convert_svd(&device, NamespaceMode::BlockWithRegsVals).context("svd2ir::convert_svd")?;

    // Equivalent to chiptool's private `clean_up_ir`.
    chiptool::transform::clean_descriptions::CleanDescriptions {}
        .run(&mut ir)
        .context("clean_descriptions")?;

    for t in input.transforms {
        apply_transform_file(&mut ir, t)?;
    }

    // Pascal case for types and enum variants, snake case for fields. Run
    // after the transforms, because their regexes match raw SVD UPPER_SNAKE names.
    chiptool::transform::sanitize::Sanitize::default()
        .run(&mut ir)
        .context("Sanitize")?;

    let dev_key = ir
        .devices
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| anyhow!("no device in IR"))?;
    let dev = ir.devices.get_mut(&dev_key).ok_or_else(|| anyhow!("no device in IR"))?;
    dev.interrupts = input
        .interrupts
        .iter()
        .map(|i| chiptool::ir::Interrupt {
            name: i.name.to_string(),
            description: i.description.map(|s| s.to_string()),
            value: i.value,
        })
        .collect();
    dev.interrupts
        .sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));

    let opts = Options::default()
        .with_common_module(CommonModule::Builtin)
        .with_defmt(DefmtOption::Feature("defmt".to_owned()))
        // The output is `include!()`d into a lib.rs that already sets
        // `#![no_std]`. `strip_crate_inner_attrs` removes the other inner attributes.
        .with_skip_no_std(true);

    // `run_gen` uses only `device_x`. The PAC modules come from
    // `data/registers/*.yaml`. Render lib.rs best-effort, so that a chiptool
    // enum check on the raw SVD does not block device.x. Example: LESENSE
    // PRSACT has 12 overlapping variants in a 3-bit field.
    let lib_rs = match generate::render(&ir, &opts) {
        Ok(tokens) => strip_crate_inner_attrs(&tokens.to_string()),
        Err(e) => {
            eprintln!("  note: skipping unused raw-SVD lib.rs render for device.x ({e:#})");
            String::new()
        }
    };

    let dev = ir.devices.get(&dev_key).ok_or_else(|| anyhow!("no device in IR"))?;
    let device_x = generate::render_device_x(&ir, dev).context("render_device_x")?;

    Ok(Generated { lib_rs, device_x })
}

/// Strip leading inner attributes `# ! [...]` from the rendered token string.
///
/// Inner attributes are illegal in an `include!()`d file. The parent lib.rs
/// sets the same allows. Token-stream `to_string()` puts a space between
/// tokens, so an attribute looks like `# ! [allow (... )]`.
fn strip_crate_inner_attrs(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    loop {
        // Skip whitespace.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Look for `# ! [` (with possible spaces).
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
        // Find matching closing `]` (no nesting expected for these attrs).
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
        // Loop again to consume more inner attrs.
    }
    s[i..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::strip_crate_inner_attrs;

    #[test]
    fn strips_chiptool_style_inner_attrs() {
        let input = "# ! [allow (non_camel_case_types)] # ! [allow (non_snake_case)] # ! [no_std] pub enum Interrupt { A = 0 , }";
        let out = strip_crate_inner_attrs(input);
        assert!(out.starts_with("pub enum Interrupt"), "got: {out}");
    }

    #[test]
    fn keeps_outer_attrs_intact() {
        // Outer attrs (no `!`) must be preserved.
        let input = "# [derive (Debug)] pub struct Foo;";
        let out = strip_crate_inner_attrs(input);
        assert_eq!(out, input);
    }

    #[test]
    fn handles_no_inner_attrs() {
        let input = "pub fn x() {}";
        let out = strip_crate_inner_attrs(input);
        assert_eq!(out, input);
    }
}
