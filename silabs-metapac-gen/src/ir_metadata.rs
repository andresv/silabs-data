//! Emit `silabs-metapac/src/metadata.rs` (IR type definitions) and the
//! per-kind `silabs-metapac/src/registers/<kind>_<version>.rs` files
//! (static `REGISTERS` constants).
//!
//! Mirrors `stm32-metapac/src/metadata.rs` and
//! `stm32-metapac/src/registers/*.rs`. The metadata module exposes the
//! chiptool IR shape as `&'static` types so HALs and tooling can
//! introspect peripheral layouts at runtime without re-parsing the
//! source YAML.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use chiptool::ir::{
    Access, Array, BitOffset, Block, BlockItem, BlockItemInner, Enum, EnumVariant, Field, FieldSet, IR,
};

use crate::pac::{IpKey, module_name_from_key};

/// The type-definitions module written to `<metapac>/src/metadata.rs`.
///
/// Copied verbatim from `stm32-metapac/src/metadata.rs` (the `pub mod ir`
/// portion). The stm32 `Metadata`/`Peripheral`/`Pin` types are stm32-
/// specific and not included — we may grow our own later, but the IR
/// portion alone is sufficient to consume the per-kind register modules
/// emitted by [`write_registers_dir`].
const METADATA_RS: &str = include_str!("../static/metadata.rs");

/// Write `<out_dir>/src/metadata.rs`.
pub fn write_metadata_module(out_dir: &Path) -> Result<()> {
    let out = out_dir.join("src").join("metadata.rs");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, METADATA_RS).with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

/// Emit one Rust file per `(kind, version)` IR under `<out_dir>` as
/// `<kind>_<version>.rs`, each containing:
///
/// ```ignore
/// use crate::metadata::ir::*;
/// pub(crate) static REGISTERS: IR = IR { blocks: &[...], ... };
/// ```
pub fn write_registers_dir(irs: &BTreeMap<IpKey, IR>, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;

    for (key, ir) in irs {
        let body = render_ir(ir);
        let path = out_dir.join(format!("{}.rs", module_name_from_key(key)));
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        crate::pac::rustfmt_in_place(&path).with_context(|| format!("rustfmt {}", path.display()))?;
    }
    Ok(())
}

// --- Render ----------------------------------------------------------------

fn render_ir(ir: &IR) -> String {
    let mut s = String::new();
    s.push_str("use crate::metadata::ir::*;\n\n");
    s.push_str("pub static REGISTERS: IR = IR {\n");

    s.push_str("    blocks: &[\n");
    // Sorted iteration: chiptool IR.blocks is a BTreeMap, already sorted.
    for (name, block) in &ir.blocks {
        render_block(&mut s, name, block);
    }
    s.push_str("    ],\n");

    s.push_str("    fieldsets: &[\n");
    for (name, fs) in &ir.fieldsets {
        render_fieldset(&mut s, name, fs);
    }
    s.push_str("    ],\n");

    s.push_str("    enums: &[\n");
    for (name, en) in &ir.enums {
        render_enum(&mut s, name, en);
    }
    s.push_str("    ],\n");

    s.push_str("};\n");
    s
}

fn render_block(s: &mut String, name: &str, block: &Block) {
    let _ = writeln!(s, "        Block {{");
    let _ = writeln!(s, "            name: {},", quote_str(name));
    let _ = writeln!(s, "            extends: {},", quote_opt(block.extends.as_deref()));
    let _ = writeln!(
        s,
        "            description: {},",
        quote_opt(block.description.as_deref())
    );
    let _ = writeln!(s, "            items: &[");
    for item in &block.items {
        render_block_item(s, item);
    }
    let _ = writeln!(s, "            ],");
    let _ = writeln!(s, "        }},");
}

fn render_block_item(s: &mut String, item: &BlockItem) {
    let _ = writeln!(s, "                BlockItem {{");
    let _ = writeln!(s, "                    name: {},", quote_str(&item.name));
    let _ = writeln!(
        s,
        "                    description: {},",
        quote_opt(item.description.as_deref())
    );
    let _ = writeln!(s, "                    array: {},", render_array(item.array.as_ref()));
    let _ = writeln!(s, "                    byte_offset: {},", item.byte_offset);
    let _ = writeln!(s, "                    inner: {},", render_inner(&item.inner));
    let _ = writeln!(s, "                }},");
}

fn render_inner(inner: &BlockItemInner) -> String {
    match inner {
        BlockItemInner::Block(b) => {
            format!(
                "BlockItemInner::Block(BlockItemBlock {{ block: {} }})",
                quote_str(&b.block)
            )
        }
        BlockItemInner::Register(r) => {
            format!(
                "BlockItemInner::Register(Register {{ access: {}, bit_size: {}, fieldset: {} }})",
                render_access(&r.access),
                r.bit_size,
                quote_opt(strip_regs_prefix(r.fieldset.as_deref()).as_deref()),
            )
        }
    }
}

fn render_access(a: &Access) -> &'static str {
    match a {
        Access::ReadWrite => "Access::ReadWrite",
        Access::Read => "Access::Read",
        Access::Write => "Access::Write",
    }
}

fn render_array(arr: Option<&Array>) -> String {
    match arr {
        None => "None".into(),
        Some(Array::Regular(r)) => format!(
            "Some(Array::Regular(RegularArray {{ len: {}, stride: {} }}))",
            r.len, r.stride
        ),
        Some(Array::Cursed(c)) => {
            let mut s = String::from("Some(Array::Cursed(CursedArray { offsets: &[");
            for (i, off) in c.offsets.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let _ = write!(s, "{off}");
            }
            s.push_str("] }))");
            s
        }
    }
}

fn render_fieldset(s: &mut String, name: &str, fs: &FieldSet) {
    let _ = writeln!(s, "        FieldSet {{");
    let _ = writeln!(s, "            name: {},", quote_str(&strip_regs_prefix_owned(name)));
    let _ = writeln!(
        s,
        "            extends: {},",
        quote_opt(strip_regs_prefix(fs.extends.as_deref()).as_deref())
    );
    let _ = writeln!(s, "            description: {},", quote_opt(fs.description.as_deref()));
    let _ = writeln!(s, "            bit_size: {},", fs.bit_size);
    let _ = writeln!(s, "            fields: &[");
    for f in &fs.fields {
        render_field(s, f);
    }
    let _ = writeln!(s, "            ],");
    let _ = writeln!(s, "        }},");
}

fn render_field(s: &mut String, f: &Field) {
    let _ = writeln!(s, "                Field {{");
    let _ = writeln!(s, "                    name: {},", quote_str(&f.name));
    let _ = writeln!(
        s,
        "                    description: {},",
        quote_opt(f.description.as_deref())
    );
    let _ = writeln!(
        s,
        "                    bit_offset: {},",
        render_bit_offset(&f.bit_offset)
    );
    let _ = writeln!(s, "                    bit_size: {},", f.bit_size);
    let _ = writeln!(s, "                    array: {},", render_array(f.array.as_ref()));
    let _ = writeln!(
        s,
        "                    enumm: {},",
        quote_opt(strip_vals_prefix(f.enumm.as_deref()).as_deref())
    );
    let _ = writeln!(s, "                }},");
}

fn render_bit_offset(b: &BitOffset) -> String {
    match b {
        BitOffset::Regular(off) => {
            format!("BitOffset::Regular(RegularBitOffset {{ offset: {off} }})")
        }
        BitOffset::Cursed(ranges) => {
            let mut s = String::from("BitOffset::Cursed(CursedBitOffset { ranges: &[");
            for (i, r) in ranges.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let _ = write!(s, "{}..={}", r.start(), r.end());
            }
            s.push_str("] })");
            s
        }
    }
}

fn render_enum(s: &mut String, name: &str, en: &Enum) {
    let _ = writeln!(s, "        Enum {{");
    let _ = writeln!(s, "            name: {},", quote_str(&strip_vals_prefix_owned(name)));
    let _ = writeln!(s, "            description: {},", quote_opt(en.description.as_deref()));
    let _ = writeln!(s, "            bit_size: {},", en.bit_size);
    let _ = writeln!(s, "            variants: &[");
    for v in &en.variants {
        render_variant(s, v);
    }
    let _ = writeln!(s, "            ],");
    let _ = writeln!(s, "        }},");
}

fn render_variant(s: &mut String, v: &EnumVariant) {
    let _ = writeln!(s, "                EnumVariant {{");
    let _ = writeln!(s, "                    name: {},", quote_str(&v.name));
    let _ = writeln!(
        s,
        "                    description: {},",
        quote_opt(v.description.as_deref())
    );
    let _ = writeln!(s, "                    value: {},", v.value);
    let _ = writeln!(s, "                }},");
}

// --- String helpers --------------------------------------------------------

fn quote_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn quote_opt(s: Option<&str>) -> String {
    match s {
        None => "None".into(),
        Some(v) => format!("Some({})", quote_str(v)),
    }
}

/// Chiptool stores fieldset references prefixed with `regs::` (e.g.
/// `regs::Cfg`) because the rendered PAC nests them under a `regs`
/// submodule. The IR metadata is a flat lookup keyed by short name, so
/// strip the prefix.
fn strip_regs_prefix(s: Option<&str>) -> Option<String> {
    s.map(|v| v.strip_prefix("regs::").unwrap_or(v).to_string())
}

fn strip_regs_prefix_owned(s: &str) -> String {
    s.strip_prefix("regs::").unwrap_or(s).to_string()
}

fn strip_vals_prefix(s: Option<&str>) -> Option<String> {
    s.map(|v| v.strip_prefix("vals::").unwrap_or(v).to_string())
}

fn strip_vals_prefix_owned(s: &str) -> String {
    s.strip_prefix("vals::").unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_str_escapes_quotes_and_backslashes() {
        assert_eq!(quote_str("hello"), "\"hello\"");
        assert_eq!(quote_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn strip_prefixes_remove_when_present_and_keep_otherwise() {
        assert_eq!(strip_regs_prefix(Some("regs::Cfg")), Some("Cfg".into()));
        assert_eq!(strip_regs_prefix(Some("Cfg")), Some("Cfg".into()));
        assert_eq!(strip_regs_prefix(None), None);

        assert_eq!(strip_vals_prefix(Some("vals::Mode")), Some("Mode".into()));
        assert_eq!(strip_vals_prefix_owned("vals::Mode"), "Mode");
    }
}
