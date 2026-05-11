//! End-to-end SVD-to-Rust codegen for one chip's SVD via chiptool.
//!
//! Pipeline:
//! 1. Read SVD XML from disk.
//! 2. Strip `_S` TrustZone-alias peripherals (Strategy A, see [`crate::peripheral`]).
//! 3. Parse with `svd-parser` (the embassy-rs fork).
//! 4. Convert to chiptool IR.
//! 5. Apply transforms loaded from one or more YAML files.
//! 6. Render `lib.rs` token stream + `device.x` linker fragment.

use anyhow::{Context, Result, anyhow};
use chiptool::generate::{self, CommonModule, DefmtOption, Options};
use chiptool::ir::IR;
use chiptool::svd2ir::{self, NamespaceMode};
use chiptool::transform::Transform;
use std::path::Path;
use svd_parser::ValidateLevel;

pub struct GenerateInput<'a> {
    pub svd_path: &'a Path,
    /// One or more transforms YAML files. Applied in order.
    pub transforms: &'a [&'a Path],
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
    let bytes = std::fs::read(path)
        .with_context(|| format!("read transforms file {}", path.display()))?;
    let cfg: TransformConfig = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parse transforms file {}", path.display()))?;
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
    let raw = std::fs::read_to_string(input.svd_path)
        .with_context(|| format!("read SVD {}", input.svd_path.display()))?;

    let preprocessed = crate::peripheral::strip_secure_peripherals(&raw)?;

    let cfg = svd_parser::Config::default()
        .expand_properties(true)
        .validate_level(ValidateLevel::Disabled);
    let device = svd_parser::parse_with_config(&preprocessed, &cfg)
        .with_context(|| format!("parse SVD {}", input.svd_path.display()))?;

    // Use `BlockWithRegsVals` namespace mode (embassy stm32-metapac style):
    // each peripheral block gets its own module, and fieldsets/enums are split
    // into `regs::` and `vals::` submodules respectively. This prevents
    // collisions where a register and an enum share a name (e.g. WDOG `LOCK`).
    let mut ir = svd2ir::convert_svd(&device, NamespaceMode::BlockWithRegsVals)
        .context("svd2ir::convert_svd")?;

    // Equivalent to chiptool's private `clean_up_ir`.
    chiptool::transform::clean_descriptions::CleanDescriptions {}
        .run(&mut ir)
        .context("clean_descriptions")?;

    for t in input.transforms {
        apply_transform_file(&mut ir, t)?;
    }

    // Match stm32-metapac casing: Pascal blocks/fieldsets/enums, snake
    // fields/block-items, Pascal enum variants. Must run after per-block
    // transforms — their regexes target raw SVD UPPER_SNAKE names.
    chiptool::transform::sanitize::Sanitize::default()
        .run(&mut ir)
        .context("Sanitize")?;

    let opts = Options::default()
        .with_common_module(CommonModule::Builtin)
        .with_defmt(DefmtOption::Feature("defmt".to_owned()))
        // The output is `include!()`'d into a parent lib.rs that already
        // sets `#![no_std]`; suppress the inner attribute here. The
        // remaining `#![allow(...)]` inner attrs are stripped post-render.
        .with_skip_no_std(true);

    let tokens = generate::render(&ir, &opts).context("generate::render")?;
    let lib_rs = strip_crate_inner_attrs(&tokens.to_string());

    let dev = ir
        .devices
        .values()
        .next()
        .ok_or_else(|| anyhow!("no device in IR"))?;
    let device_x = generate::render_device_x(&ir, dev).context("render_device_x")?;

    Ok(Generated { lib_rs, device_x })
}

/// Strip leading inner attributes `# ! [...]` from the rendered token string.
///
/// chiptool emits a few `#![allow(non_camel_case_types)]` etc. at the top.
/// Inner attributes are illegal in an `include!()`'d file, so we drop them
/// — the parent lib.rs sets equivalent allows at crate root.
///
/// Token-stream `to_string()` separates every token with a space, so a
/// crate-level inner attribute looks like: `# ! [allow (... )]`.
/// We walk the prefix and skip those.
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
