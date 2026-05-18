pub mod ir {
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct IR {
        pub blocks: &'static [Block],
        pub fieldsets: &'static [FieldSet],
        pub enums: &'static [Enum],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct Block {
        pub name: &'static str,
        pub extends: Option<&'static str>,

        pub description: Option<&'static str>,
        pub items: &'static [BlockItem],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct BlockItem {
        pub name: &'static str,
        pub description: Option<&'static str>,

        pub array: Option<Array>,
        pub byte_offset: u32,

        pub inner: BlockItemInner,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum BlockItemInner {
        Block(BlockItemBlock),
        Register(Register),
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct Register {
        pub access: Access,
        pub bit_size: u32,
        pub fieldset: Option<&'static str>,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct BlockItemBlock {
        pub block: &'static str,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum Access {
        ReadWrite,
        Read,
        Write,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct FieldSet {
        pub name: &'static str,
        pub extends: Option<&'static str>,

        pub description: Option<&'static str>,
        pub bit_size: u32,
        pub fields: &'static [Field],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct Field {
        pub name: &'static str,
        pub description: Option<&'static str>,

        pub bit_offset: BitOffset,
        pub bit_size: u32,
        pub array: Option<Array>,
        pub enumm: Option<&'static str>,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum Array {
        Regular(RegularArray),
        Cursed(CursedArray),
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct RegularArray {
        pub len: u32,
        pub stride: u32,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct CursedArray {
        pub offsets: &'static [u32],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum BitOffset {
        Regular(RegularBitOffset),
        Cursed(CursedBitOffset),
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct RegularBitOffset {
        pub offset: u32,
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct CursedBitOffset {
        pub ranges: &'static [core::ops::RangeInclusive<u32>],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct Enum {
        pub name: &'static str,
        pub description: Option<&'static str>,
        pub bit_size: u32,
        pub variants: &'static [EnumVariant],
    }

    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct EnumVariant {
        pub name: &'static str,
        pub description: Option<&'static str>,
        pub value: u64,
    }
}

/// Chip-level metadata: peripherals, interrupts, memory regions.
///
/// Mirrors `stm32-metapac::metadata::Metadata` so HAL build scripts can
/// walk a chip's hardware inventory at build time without re-parsing the
/// per-chip JSON. The Cargo `metadata` feature exposes a chip-specific
/// `METADATA` static of this shape at `silabs_metapac::metadata::METADATA`.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Metadata {
    /// Full chip part number (matches the Cargo feature flag).
    pub name: &'static str,
    /// Cortex-M core variant string from the CMSIS pdsc
    /// (`Cortex-M33`, `Cortex-M4`, etc.).
    pub core: &'static str,
    /// Has an FPU.
    pub fpu: bool,
    /// Has an MPU.
    pub mpu: bool,
    /// Has Cortex-M TrustZone.
    pub trustzone: bool,
    pub memory: &'static [MemoryRegion],
    /// Peripheral instances, deduplicated to the non-secure alias
    /// (the `_S` TrustZone alias is dropped — secure-state code can
    /// XOR `0x0100_0000` onto the base if needed).
    pub peripherals: &'static [Peripheral],
    /// Cortex-M interrupt table from the CMSIS device header
    /// (radio IRQs included — the SVD `<interrupt>` blocks are
    /// incomplete on Silabs parts and intentionally ignored).
    pub interrupts: &'static [Interrupt],
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct MemoryRegion {
    /// Region identifier from the pdsc (`IROM1`, `IRAM1`, etc.).
    pub name: &'static str,
    /// Base address.
    pub address: u64,
    /// Region size in bytes.
    pub size: u64,
    /// Access string from the pdsc (`rx`, `rwx`, …).
    pub access: &'static str,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Peripheral {
    /// Canonical instance name, with `_NS`/`_S` suffix stripped
    /// (e.g. `TIMER0`, `EUSART1`, `GPIO`). Matches the typed const
    /// emitted at the chip module root.
    pub name: &'static str,
    /// Non-secure base address.
    pub address: u64,
    /// Routed peripheral kind (`timer`, `gpio`, `eusart`, …).
    pub kind: &'static str,
    /// Routed register-YAML version label (`v1_w`, `v7`, `v2_lf`, …).
    /// Together with `kind` this names the `<kind>_<version>` module
    /// at the metapac crate root.
    pub version: &'static str,
    /// Canonical block name inside the register YAML (`Timer`, `Gpio`).
    pub block: &'static str,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Interrupt {
    /// IRQ name from the CMSIS device header (e.g. `TIMER0`,
    /// `GPIO_ODD`).
    pub name: &'static str,
    /// NVIC vector number.
    pub number: u32,
}
