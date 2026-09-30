//! IR merging for `seed`.
//!
//! - [`merge_superset`] merges two instances of one `(kind, version, block)`
//!   that differ only by added items, fields, enum variants, fieldsets or
//!   enums. Their union becomes the YAML of the version. A name with a
//!   different offset, width or type in the two instances is a conflict.
//!   Then `seed` stops, or with `--candidates-dir` writes the conflicting
//!   IRs there.
//! - [`combine_blocks`] puts all blocks of one `(kind, version)` into one
//!   YAML (TIMER `Timer` and `Timer32`). Fieldsets and enums that are equal
//!   in all blocks are shared. The others get the block suffix
//!   (`Timer32` → `regs::Cnt32`).
//!
//! Descriptions never cause a conflict: the first IR's text wins.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chiptool::ir::{Block, BlockItem, BlockItemInner, Enum, FieldSet, IR};

/// Merge `other` into `base`. Returns how many elements `other` added, or
/// a description of the first conflict.
pub fn merge_superset(base: &mut IR, other: &IR) -> Result<usize, String> {
    let mut added = 0;
    for (name, ob) in &other.blocks {
        let Some(bb) = base.blocks.get_mut(name) else {
            base.blocks.insert(name.clone(), ob.clone());
            added += 1;
            continue;
        };
        for item in &ob.items {
            match bb.items.iter().find(|i| i.name == item.name) {
                None => {
                    bb.items.push(item.clone());
                    added += 1;
                }
                Some(b) if item_shape(b) != item_shape(item) => {
                    return Err(format!("block {name}: item `{}` differs", item.name));
                }
                Some(_) => {}
            }
        }
        bb.items.sort_by_key(|i| i.byte_offset);
    }
    for (name, of) in &other.fieldsets {
        let Some(bf) = base.fieldsets.get_mut(name) else {
            base.fieldsets.insert(name.clone(), of.clone());
            added += 1;
            continue;
        };
        if bf.bit_size != of.bit_size {
            return Err(format!("fieldset {name}: bit_size differs"));
        }
        for f in &of.fields {
            match bf.fields.iter().find(|x| x.name == f.name) {
                None => {
                    bf.fields.push(f.clone());
                    added += 1;
                }
                Some(b)
                    if (&b.bit_offset, b.bit_size, &b.array, &b.enumm)
                        != (&f.bit_offset, f.bit_size, &f.array, &f.enumm) =>
                {
                    return Err(format!("fieldset {name}: field `{}` differs", f.name));
                }
                Some(_) => {}
            }
        }
        bf.fields.sort_by(|a, b| a.bit_offset.cmp(&b.bit_offset));
    }
    for (name, oe) in &other.enums {
        let Some(be) = base.enums.get_mut(name) else {
            base.enums.insert(name.clone(), oe.clone());
            added += 1;
            continue;
        };
        if be.bit_size != oe.bit_size {
            return Err(format!("enum {name}: bit_size differs"));
        }
        for v in &oe.variants {
            match be.variants.iter().find(|x| x.name == v.name) {
                None => {
                    be.variants.push(v.clone());
                    added += 1;
                }
                Some(b) if b.value != v.value => {
                    return Err(format!("enum {name}: variant `{}` differs", v.name));
                }
                Some(_) => {}
            }
        }
        be.variants.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(added)
}

fn item_shape(i: &BlockItem) -> (u32, String, String) {
    (i.byte_offset, format!("{:?}", i.array), format!("{:?}", i.inner))
}

fn fieldset_shape(f: &FieldSet) -> String {
    let fields: Vec<_> = f
        .fields
        .iter()
        .map(|x| (&x.name, &x.bit_offset, x.bit_size, &x.array, &x.enumm))
        .collect();
    format!("{:?} {:?}", f.bit_size, fields)
}

fn enum_shape(e: &Enum) -> String {
    let variants: Vec<_> = e.variants.iter().map(|v| (&v.name, v.value)).collect();
    format!("{} {:?}", e.bit_size, variants)
}

fn block_shape(b: &Block) -> String {
    let items: Vec<_> = b.items.iter().map(|i| (&i.name, item_shape(i))).collect();
    format!("{items:?}")
}

/// Combine one IR per block of a `(kind, version)` into a single IR.
///
/// The primary block is the one whose name is a prefix of every other
/// block's name (`Timer` for `Timer` + `Timer32`), else the first by name.
/// A secondary block's fieldsets, enums and nested blocks that clash with
/// different content get the suffix: the block name minus the primary name
/// (`32`), or the whole block name when it has no such prefix.
pub fn combine_blocks(mut parts: Vec<(String, IR)>) -> Result<IR> {
    if parts.len() == 1 {
        return Ok(parts.pop().expect("one part").1);
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let primary_idx = parts
        .iter()
        .position(|(p, _)| parts.iter().all(|(o, _)| o.starts_with(p.as_str())))
        .unwrap_or(0);
    let (primary, mut out) = parts.remove(primary_idx);

    for (block, mut ir) in parts {
        let suffix = block
            .strip_prefix(primary.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&block)
            .to_owned();

        // Enums first, since fieldsets reference them.
        let mut enum_renames = BTreeMap::new();
        for (name, e) in &ir.enums {
            if out.enums.get(name).is_some_and(|b| enum_shape(b) != enum_shape(e)) {
                enum_renames.insert(name.clone(), format!("{name}{suffix}"));
            }
        }
        for f in ir.fieldsets.values_mut().flat_map(|fs| fs.fields.iter_mut()) {
            if let Some(new) = f.enumm.as_ref().and_then(|e| enum_renames.get(e)) {
                f.enumm = Some(new.clone());
            }
        }
        let mut fs_renames = BTreeMap::new();
        for (name, fs) in &ir.fieldsets {
            if out
                .fieldsets
                .get(name)
                .is_some_and(|b| fieldset_shape(b) != fieldset_shape(fs))
            {
                fs_renames.insert(name.clone(), format!("{name}{suffix}"));
            }
        }
        for item in ir.blocks.values_mut().flat_map(|b| b.items.iter_mut()) {
            if let BlockItemInner::Register(r) = &mut item.inner
                && let Some(new) = r.fieldset.as_ref().and_then(|f| fs_renames.get(f))
            {
                r.fieldset = Some(new.clone());
            }
        }
        // Nested blocks other than the top-level one.
        let mut block_renames = BTreeMap::new();
        for (name, b) in &ir.blocks {
            if *name != block && out.blocks.get(name).is_some_and(|o| block_shape(o) != block_shape(b)) {
                block_renames.insert(name.clone(), format!("{name}{suffix}"));
            }
        }
        for item in ir.blocks.values_mut().flat_map(|b| b.items.iter_mut()) {
            if let BlockItemInner::Block(b) = &mut item.inner
                && let Some(new) = block_renames.get(&b.block)
            {
                b.block = new.clone();
            }
        }

        if out.blocks.contains_key(&block) {
            bail!("block {block} exists in more than one part");
        }
        for (name, b) in ir.blocks {
            let name = block_renames.get(&name).cloned().unwrap_or(name);
            out.blocks.entry(name).or_insert(b);
        }
        for (name, fs) in ir.fieldsets {
            let name = fs_renames.get(&name).cloned().unwrap_or(name);
            out.fieldsets.entry(name).or_insert(fs);
        }
        for (name, e) in ir.enums {
            let name = enum_renames.get(&name).cloned().unwrap_or(name);
            out.enums.entry(name).or_insert(e);
        }
    }
    chiptool::transform::sort::Sort {}
        .run(&mut out)
        .context("sort combined IR")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ir(yaml: &str) -> IR {
        serde_yaml::from_str(yaml).unwrap()
    }

    const NARROW: &str = "
block/Timer:
  items:
  - name: ctrl
    byte_offset: 0
    fieldset: regs::Ctrl
  - name: cnt
    byte_offset: 4
    fieldset: regs::Cnt
fieldset/regs::Ctrl:
  fields:
  - name: en
    bit_offset: 0
    bit_size: 1
fieldset/regs::Cnt:
  fields:
  - name: cnt
    bit_offset: 0
    bit_size: 16
";

    #[test]
    fn superset_adds_items_fields_and_fieldsets() {
        let mut base = ir(NARROW);
        let other = ir("
block/Timer:
  items:
  - name: ctrl
    byte_offset: 0
    fieldset: regs::Ctrl
  - name: lf
    byte_offset: 8
    fieldset: regs::Lf
fieldset/regs::Ctrl:
  fields:
  - name: en
    description: Different text is fine.
    bit_offset: 0
    bit_size: 1
  - name: wu
    bit_offset: 3
    bit_size: 1
fieldset/regs::Lf:
  fields:
  - name: lfen
    bit_offset: 0
    bit_size: 1
");
        assert_eq!(merge_superset(&mut base, &other), Ok(3));
        let items: Vec<_> = base.blocks["Timer"].items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(items, ["ctrl", "cnt", "lf"]);
        assert_eq!(base.fieldsets["regs::Ctrl"].fields.len(), 2);
        assert!(base.fieldsets["regs::Ctrl"].fields[0].description.is_none());
    }

    #[test]
    fn superset_rejects_moved_or_resized_fields() {
        let mut base = ir(NARROW);
        let wide = ir(&NARROW.replace("bit_size: 16", "bit_size: 32"));
        let err = merge_superset(&mut base, &wide).unwrap_err();
        assert!(err.contains("regs::Cnt") && err.contains("`cnt`"), "{err}");
    }

    #[test]
    fn combine_suffixes_only_the_differing_fieldsets() {
        let narrow = ir(NARROW);
        let wide = ir(&NARROW
            .replace("Timer:", "Timer32:")
            .replace("bit_size: 16", "bit_size: 32"));
        let out = combine_blocks(vec![("Timer32".into(), wide), ("Timer".into(), narrow)]).unwrap();
        let fs = |b: &str, reg: &str| match &out.blocks[b].items.iter().find(|i| i.name == reg).unwrap().inner {
            BlockItemInner::Register(r) => r.fieldset.clone().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(fs("Timer", "cnt"), "regs::Cnt");
        assert_eq!(fs("Timer32", "cnt"), "regs::Cnt32");
        assert_eq!(fs("Timer32", "ctrl"), "regs::Ctrl");
        assert_eq!(out.fieldsets["regs::Cnt32"].fields[0].bit_size, 32);
        assert!(!out.fieldsets.contains_key("regs::Ctrl32"));
    }
}
