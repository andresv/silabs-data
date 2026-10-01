// `pub mod ir` is copied from stm32-metapac (https://github.com/embassy-rs/stm32-data), MIT OR Apache-2.0.
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

/// Silicon Labs chip generation, from the SDK's
/// `_SILICON_LABS_32B_SERIES_<N>_CONFIG_<M>` macro pair.
///
/// Direct dependents also get it as cfgs: `#[cfg(silabs_series_2_config = "3")]`.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Series {
    /// Series 0 (Cortex-M0+/M3, EFM32 Gecko families G/GG/LG/TG/WG/ZG/HG).
    /// Series 0 headers define no config number.
    Series0,
    /// Series 1 (Cortex-M4, EFM32xG1x / EFR32xG1x). The config number is
    /// not unique per family: GG11B, TG11B, PG1B and JG1B all report 1.
    /// Use the chip family, not only the config, to select HAL code.
    Series1(u8),
    /// Series 2 (Cortex-M33 + TrustZone, ~2020-2024). Within-series
    /// config 1..9, one per chip family: xG21=1, xG22=2, xG23=3,
    /// xG24=4, FG25=5, xG26=6, xG27=7, xG28=8, xG29=9.
    Series2(u8),
    /// Series 3 (~2024+, `SI`-prefixed chips). Within-series config
    /// 301+ per Silicon Labs' new 3-digit numbering.
    Series3(u16),
}

/// Chip metadata for HAL build scripts.
///
/// The `metadata` feature exports the active chip's value as
/// `silabs_metapac::metadata::METADATA`.
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
    /// Silicon Labs chip generation + within-series config number.
    /// See [`Series`].
    pub series: Series,
    /// Number of NVIC priority bits (`__NVIC_PRIO_BITS` in the CMSIS
    /// device header): 2 on Cortex-M0+, 3 on Series 0/1 M3/M4, 4 on M33.
    pub nvic_priority_bits: u8,
    pub memory: &'static [MemoryRegion],
    /// Peripheral instances, with paired TrustZone aliases sharing one
    /// register-layout entry. Both exact SVD addresses remain available on
    /// [`Peripheral`].
    pub peripherals: &'static [Peripheral],
    /// Cortex-M interrupt table from the CMSIS device header, radio IRQs
    /// included.
    pub interrupts: &'static [Interrupt],
    /// Bonded GPIO pins, sorted by port, then pin. Series 0 headers have no
    /// pin masks, so a Series 0 chip lists 16 pins on each GPIO port.
    pub pins: &'static [Pin],
    /// Number of DMA channels (`LDMA_CH_NUM` / `DMA_CHAN_COUNT`). 0 when the chip has no DMA.
    pub dma_channel_count: u8,
}

/// One GPIO pin.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Pin {
    /// Port letter and pin number, as Silicon Labs names the pin: two
    /// digits on Series 2 (`PA00`), no padding on Series 0 (`PA0`, `PE10`).
    pub name: &'static str,
    pub port: u8,
    pub pin: u8,
}

/// One memory region.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct MemoryRegion {
    /// Region identifier from the pdsc (`IROM1`, `IRAM1`, etc.).
    pub name: &'static str,
    pub kind: MemoryRegionKind,
    /// Base address.
    pub address: u64,
    /// Region size in bytes.
    pub size: u64,
    /// Erase and write geometry. `Some` only for flash.
    pub settings: Option<FlashSettings>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum MemoryRegionKind {
    Flash,
    Ram,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct FlashSettings {
    /// Erase page size in bytes (`FLASH_PAGE_SIZE` in the device header).
    pub erase_size: u32,
    /// Smallest write unit in bytes. The MSC writes one 32-bit word at a time.
    pub write_size: u32,
    /// Value of an erased byte.
    pub erase_value: u8,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Peripheral {
    /// Canonical instance name. A trailing `_NS` is stripped (e.g. `GPIO_NS`
    /// becomes `GPIO`); infix names such as `SEMAILBOX_NS_HOST` are retained.
    /// Matches the canonical typed const emitted at the chip module root.
    pub name: &'static str,
    /// Non-secure base address.
    pub address: u64,
    /// Secure alias base address from the SVD, when this peripheral has a
    /// paired TrustZone `_S`/`_S_` instance.
    pub secure_address: Option<u64>,
    /// Routed peripheral kind (`timer`, `gpio`, `eusart`, …).
    pub kind: &'static str,
    /// Routed register-YAML version label (`s2v1`, `s2v7`, `s0v1`, …).
    /// Together with `kind` this names the `<kind>_<version>` module
    /// at the metapac crate root.
    pub version: &'static str,
    /// Canonical block name inside the register YAML (`Timer`, `Gpio`).
    pub block: &'static str,
    /// Register layout of `<kind>_<version>`. Build scripts use it to find
    /// fields and enums.
    pub ir: &'static ir::IR,
    /// CMU data. `None` when the peripheral has neither a clock gate nor a
    /// kernel clock (CMU, EMU, DEVINFO, ...).
    pub cmu: Option<PeripheralCmu>,
    /// IRQs of the peripheral.
    pub interrupts: &'static [PeripheralInterrupt],
    /// DMA request signals of the peripheral.
    pub dma_requests: &'static [PeripheralDmaRequest],
}

/// One IRQ of a peripheral.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct PeripheralInterrupt {
    /// Signal name: `GLOBAL` when the IRQ belongs to the whole peripheral, else the IRQ name suffix (`RX`, `APP`, `ODD`).
    pub signal: &'static str,
    /// IRQ name, as in `Metadata.interrupts`.
    pub interrupt: &'static str,
}

/// One DMA request signal of a peripheral.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct PeripheralDmaRequest {
    /// Signal name without the peripheral prefix: `RXFL`, `TXFL`, `RXDATAV`, `CC0`.
    pub signal: &'static str,
    /// SOURCESEL value (LDMAXBAR `CH_REQSEL` on Series 2, DMA `CH_CTRL` on Series 0).
    pub sourcesel: u8,
    /// SIGSEL value.
    pub sigsel: u8,
}

/// CMU data of one peripheral.
///
/// There is no bus clock or reset entry: Silicon Labs chips have no
/// per-peripheral reset bits.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct PeripheralCmu {
    /// Clock-gate bit that turns the peripheral's bus clock on. `None`
    /// when the peripheral is always clocked.
    pub enable: Option<ClockEnable>,
    /// Clock that drives the peripheral (baud rate, counter, sampling).
    /// `None` when the peripheral has no kernel clock of its own.
    pub kernel_clock: Option<PeripheralCmuKernelClock>,
}

/// Source of a peripheral's kernel clock.
#[derive(Debug, Eq, PartialEq, Clone)]
pub enum PeripheralCmuKernelClock {
    /// A fixed clock-tree node, in lowercase: `em01grpaclk`, `pclk`,
    /// `lspclk`, `hfperclk`, ... Group clocks such as EM01GRPACLK have
    /// their own mux, which the HAL sets when it sets up the clock tree.
    Clock(&'static str),
    /// A CMU select field of this peripheral (`eusart0clkctrl.clksel`).
    /// The field has an enum. Each variant names a source clock, in the
    /// same lowercase form as [`PeripheralCmuKernelClock::Clock`]. A `rt`
    /// suffix (`Hfxort`) is the retimed copy of the same clock, and
    /// `Hclkdiv1024` is HCLK divided by 1024.
    Mux(PeripheralCmuRegister),
}

/// A field in a CMU register.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct PeripheralCmuRegister {
    /// Register name as in the register YAML (`eusart0clkctrl`).
    pub register: &'static str,
    /// Field name (`clksel`).
    pub field: &'static str,
}

/// One CMU clock-gate bit.
///
/// To set it without a read-modify-write race:
/// - Series 2: write `1 << bit` to `address + 0x1000` (the register's SET
///   alias).
/// - Series 0 (Cortex-M3): write `1` to the bit-band alias word
///   `0x4200_0000 + (address - 0x4000_0000) * 32 + bit * 4`.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ClockEnable {
    /// CMU register name (`clken1`, `hfperclken0`).
    pub register: &'static str,
    /// Field name in that register (`timer4`).
    pub field: &'static str,
    /// Absolute address of the non-secure CMU register.
    pub address: u64,
    /// Bit number of the field.
    pub bit: u32,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Interrupt {
    /// IRQ name from the CMSIS device header (e.g. `TIMER0`,
    /// `GPIO_ODD`).
    pub name: &'static str,
    /// NVIC vector number.
    pub number: u32,
}
