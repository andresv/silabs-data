//! Per-peripheral SVD-to-IR extraction.
//!
//! Takes one `svd::Peripheral` and produces a chiptool `IR` containing just
//! that peripheral's block, fieldsets and enums. The block name is
//! canonicalised to the perimap-routed `block` (e.g. `Eusart`, `Gpio`) before
//! extraction so every chip emitting the same `(kind, version)` produces a
//! structurally identical IR.

use anyhow::{Context, Result};
use chiptool::ir::IR;
use chiptool::svd2ir::NamespaceMode;
use chiptool::transform::Transform;
use std::path::Path;

/// Strip a `<prefix>::` segment from every block / fieldset / enum name in
/// `ir`, *and* from references inside blocks. Keys that don't start with the
/// prefix are left alone.
fn strip_block_prefix(ir: &mut chiptool::ir::IR, prefix: &str) {
    let head = format!("{prefix}::");
    let strip = |s: &mut String| {
        if let Some(rest) = s.strip_prefix(&head) {
            *s = rest.to_owned();
        }
    };
    fn rekey<V>(map: &mut std::collections::BTreeMap<String, V>, head: &str) {
        let keys: Vec<String> = map.keys().cloned().collect();
        for k in keys {
            if let Some(new_k) = k.strip_prefix(head) {
                let v = map.remove(&k).unwrap();
                map.insert(new_k.to_owned(), v);
            }
        }
    }
    rekey(&mut ir.blocks, &head);
    rekey(&mut ir.fieldsets, &head);
    rekey(&mut ir.enums, &head);

    use chiptool::ir::{BlockItemInner, FieldSet};
    for block in ir.blocks.values_mut() {
        if let Some(ext) = &mut block.extends {
            strip(ext);
        }
        for item in &mut block.items {
            match &mut item.inner {
                BlockItemInner::Register(r) => {
                    if let Some(fs) = &mut r.fieldset {
                        strip(fs);
                    }
                }
                BlockItemInner::Block(b) => strip(&mut b.block),
            }
        }
    }
    for fs in ir.fieldsets.values_mut() {
        let FieldSet { extends, fields, .. } = fs;
        if let Some(ext) = extends {
            strip(ext);
        }
        for f in fields {
            if let Some(e) = &mut f.enumm {
                strip(e);
            }
        }
    }
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

/// Extract one peripheral into IR with the block name canonicalised to
/// `block`. Applies the per-kind transforms after extraction.
///
/// `transforms_dir` is the directory of per-kind YAMLs. We look for
/// `<BLOCK>.yaml` and (optionally) `<BLOCK>_<version>.yaml`. A missing file
/// is silently skipped.
pub fn extract_ip(
    peripheral: &svd_parser::svd::Peripheral,
    block: &str,
    version: &str,
    transforms_dir: &Path,
) -> Result<IR> {
    let mut p = peripheral.clone();
    p.header_struct_name = Some(block.to_owned());
    // Scrub per-instance description (e.g. `ACMP0_NS Registers` vs
    // `ACMP1_NS Registers`) so two instances of the same `(kind, version)`
    // produce structurally identical IRs.
    p.description = Some(format!("{block} peripheral."));

    let mut ir = chiptool::commands::extract_peripheral(&p, NamespaceMode::BlockWithRegsVals)
        .with_context(|| format!("extract_peripheral for block {block}"))?;
    strip_block_prefix(&mut ir, block);

    // Apply per-block transform (e.g. transforms/GPIO.yaml). Missing file is OK.
    let block_yaml = transforms_dir.join(format!("{block}.yaml"));
    if block_yaml.is_file() {
        apply_transform_file(&mut ir, &block_yaml)
            .with_context(|| format!("apply {}", block_yaml.display()))?;
    }
    // Apply per-(block, version) override if present.
    let bv_yaml = transforms_dir.join(format!("{block}_{version}.yaml"));
    if bv_yaml.is_file() {
        apply_transform_file(&mut ir, &bv_yaml)
            .with_context(|| format!("apply {}", bv_yaml.display()))?;
    }

    // Prune trivially useless on/off enums (DISABLE/ENABLE, DIS/EN, OFF/ON,
    // etc.). `soft: false` removes both the field references *and* the enum
    // definitions; with `soft: true` the orphan enum definitions linger in
    // the YAML, bloating downstream Rust output. Matches stm32-data's
    // per-peripheral `!DeleteEnums from: ^(...)$` intent at a generic level.
    chiptool::transform::delete_useless_enums::DeleteUselessEnums { soft: false }
        .run(&mut ir)
        .context("DeleteUselessEnums")?;

    // Final sort to keep the YAML output deterministic.
    chiptool::transform::sort::Sort {}
        .run(&mut ir)
        .context("final sort")?;

    Ok(ir)
}
