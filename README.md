# silabs-data

`silabs-data` is a project that generates clean, machine-readable register data for Silicon Labs EFR32 / EFM32 / Gecko microcontrollers. It also provides a tool to generate a Rust Peripheral Access Crate (`silabs-metapac`) for these devices. The pipeline and curation policy mirror [`embassy-rs/stm32-data`](https://github.com/embassy-rs/stm32-data) — we treat that project as the reference design.

The generated data currently includes:

- Base chip information (RAM, flash, package, memory map)
- Peripheral addresses and interrupts
- Cortex-M interrupt vector tables (via `cortex-m-rt`)
- Register blocks for all peripherals, shared across chips by `(kind, version)`

## Generated PAC crate

The PAC is regenerated locally into `build/silabs-metapac/`. To use it from another workspace:

```toml
[dependencies]
silabs-metapac = { path = "../silabs-data/build/silabs-metapac" }
```

One feature per OPN gates the chip's content; pick exactly one. Enable the `rt` feature for `cortex-m-rt` interrupt-vector glue.

### Peripheral cfgs

The metapac gives each direct dependent one `peri_<name>` cfg per peripheral instance. The name is the lowercase `METADATA.peripherals` name. Examples are `peri_timer0`, `peri_eusart1` and `peri_gpio`. A TrustZone pair (`GPIO_NS` and `GPIO_S`) gives one cfg.

### Memory regions

Each `METADATA.memory` entry has a `kind`, `Flash` or `Ram`. The pdsc region name sets the kind: `IROM*` is flash and `IRAM*` is RAM. A flash region has `settings` with the erase size, the write size and the erase value. The erase size is `FLASH_PAGE_SIZE` from the device header. The write size is 4 bytes. The erase value is `0xFF`. A RAM region has no settings.

### Peripheral interrupts

Each `METADATA.peripherals` entry has an `interrupts` list. Each item has a `signal` and an `interrupt`. The `interrupt` is the IRQ name in `METADATA.interrupts`. The generator attaches an IRQ to a peripheral with these rules:

1. An IRQ with the same name as a peripheral gets the signal `GLOBAL`. For example, `TIMER0` gets `(GLOBAL, TIMER0)`.
2. An IRQ named `<peripheral>_<suffix>` gets the signal `<suffix>`. For example, `EUSART0` gets `(RX, EUSART0_RX)`. When two peripheral names match, the longer name wins. Thus `TIMER10_X` never goes to `TIMER1`.
3. `OVERRIDES` in `silabs-data-gen/src/interrupts.rs` attaches the IRQs that match no name. For example, `SYSRTC0` gets `(APP, SYSRTC_APP)`.

All other IRQs have no peripheral. Radio, software, kernel, CTI and bus-bridge IRQs are in this group.

### Pins

`METADATA.pins` lists the GPIO pins that the package bonds. Each pin has a `name`, a `port` and a `pin` number. The name uses the Silicon Labs format of the series. Series 2 names have a two-digit pin number (`PA00`). Series 0 names have no zero padding (`PA0`, `PE10`). On Series 2, the `GPIO_Px_MASK` defines in the device header give the pins. Series 0 headers have no pin masks. Thus on Series 0, the `packages` key in `data/pins/efm32gg.yaml` gives the pins. Two Series 0 chips have no pin data (see below). Each of them lists 16 pins for each GPIO port, and `gen` prints one warning for each.

Each pin also has `em2`. `Some(true)` means that the pin keeps working in EM2. On Series 2, the value comes from the `em2` key in `data/pins/<family>.yaml`. On Series 0, the value is `None`, because the pin tool has no EM2 facts for Series 0.

### Peripheral pins

Each `METADATA.peripherals` entry has a `pins` list. The list has one entry for each signal and route. Each entry has a `signal`, a `route` and a `pins` list. The signal name has no peripheral prefix (`TX`, `SDA`, `CC0`). The pin names use the vendor format of the series (`PA05` or `PA5`). The list has only bonded pins, sorted by port, then by pin number. The entries are sorted by signal, then by route: `Location` by number, `Analog` by bus name.

For example, MG24 `EUSART0` has one `TX` entry. Its route is `Dbus`, and its pins are `PA00` to `PA09` and `PB00` to `PB05`. A Series 0 `USART0` has one `TX` entry for each location, with one pin each.

The `route` tells a HAL how to connect the pin:

- `Dbus { register, enable }` (Series 2): write the port and pin to `GPIO.<register>`. Then set `GPIO.<enable.register>.<enable.field>`.
- `Location { location, enable }` (Series 0): write `location` to the LOCATION field of the peripheral `route` register. Then set the enable field of the same register. All signals of a peripheral share one LOCATION. A peripheral whose `route` register has no LOCATION field (`LESENSE`, `USB`) has one location, 0.
- `Analog { bus }` (Series 2): the pin connects through the analog bus of its port. Port A uses `ABUS`, port B uses `BBUS`, and ports C and D use `CDBUS`. Allocate the pin in `GPIO.<bus>ALLOC`. The field depends on the pin parity (even or odd). The pin tool calls every bus `ABUS`, so the files keep `bus: ABUS`. `gen` chooses the bus from the port, and stops when the GPIO register YAML has no `<bus>alloc` register.
- `Fixed { enable, alternative }`: the pin is fixed. There is no route register. `enable` is the field that connects the pin, when there is one: Series 0 USB `DM`, `DP` and `ID` use `route.phypen`, Series 0 DAC0 `OUT0ALT`, `OUT1ALT` and `OUT2` use `opa0mux.outpen`, `opa1mux.outpen` and `opa2mux.outpen`, and the Series 2 debug and trace pins use `GPIO.dbgroutepen` and `GPIO.traceroutepen`. `FIXED_ENABLES` in `silabs-data-gen/src/pins.rs` lists them. `alternative` is the number of a numbered fixed alternative, for example Series 0 DAC0 `OUT0ALT` 0 to 4. For DAC0, set bit `alternative` of the `outpen` field.

A `Dbus` or `Location` route has `enable: None` only when the signal is in the `NO_ENABLE` list in `silabs-data-gen/src/pins.rs`. Each entry gives the reason, for example "input-only" for `CTS`, or "the multi-bit field `apen` enables a range of address lines" for EBI `A00` to `A27`. `gen` stops on any other missing enable. It names the chip, the peripheral and the signal. It also stops when a `NO_ENABLE` signal has an enable, because then the entry is stale.

The source is `data/pins/<family>.yaml`. We extracted these files once from the Silicon Labs pin tool with `./d extract-pins`. Now we maintain them by hand, like `data/registers/`. `./d gen-all` never reads pin-tool files. `gen` takes the family file whose name is the exact `family` of the chip (`EFR32MG24`, `EFM32GG`). Each file gives one form for each signal:

- `{ ports: [A, B] }`: every bonded pin of these ports. This gives one `Dbus` entry.
- `{ pins: [PD01] }`: these fixed pins, when the chip bonds them. This gives one `Fixed` entry.
- `{ alternatives: { 0: PC0, 1: PC1 } }`: numbered fixed alternatives. This gives one `Fixed` entry for each bonded alternative.
- `{ bus: ABUS }`: every bonded pin, through an analog bus. This gives one `Analog` entry for each bus.
- `{ locations: { 0: PE10, 1: PE7 } }` (Series 0): the LOCATION value of each pin. This gives one `Location` entry for each location whose pin is bonded.

A Series 2 file has an `em2` key with whole `ports` and single `pins`. A Series 0 file has a `packages` key with the bonded pins of each group of parts. Series 2 takes its bonded pins from the device headers.

The route register is `GPIO.<inst>_<sig>route`, and the enable bit is `GPIO.<inst>_routeen.<sig>pen`. `<inst>` and `<sig>` are the lowercase peripheral and signal names. `ROUTE_EXCEPTIONS` in `silabs-data-gen/src/pins.rs` lists the names that do not follow this rule, for example `letimer_out0route` for `LETIMER0`. `gen` stops when a route register is not in the GPIO register YAML. A Series 2 signal with a route register must use the `ports` form. `gen` stops when the file gives `pins` for it, because a hand edit lost the route.

Some data has no entries:

- `efm32gg900f1024` and `efm32gg900f512` have no peripheral pins. They are die parts, and the pin tool has no data for them. `gen` prints one warning for each.
- A peripheral in the pin file that is not on the chip gets no entries. `gen` prints the set for each chip. Examples are `MODEM` and `PTI` on Series 2, and `BU` on Series 0. `BU` (`BU_VIN`, `BU_VOUT`, `BU_STAT`) holds the backup power pins, which the EMU controls. They are not signals of a peripheral.
- `PERIPHERAL_NAMES` in `silabs-data-gen/src/pins.rs` moves the entries of a pin-file peripheral to another chip peripheral. On Series 0, the CMU controls the crystal oscillators, so the `HFXO` and `LFXO` pins are fixed pins of `CMU`. Their signals keep the vendor names: `HFXO_N`, `HFXO_P`, `LFXO_N` and `LFXO_P` (EFM32GG: PB14, PB13, PB8 and PB7). On Series 2, `HFXO0` and `LFXO` are peripherals, so their pins stay with them.
- Series 0 `DBG` and `ETM` get no entries. Their pins route through `GPIO.ROUTE` (the `SWLOCATION` and `ETMLOCATION` fields), and the metadata does not model this. `gen` prints them in a separate set.
- Some MG24 and MG26 parts have dedicated analog pads (`AIN0`, `AIN1`) for `IADC0`. They are not GPIO pins, so the files do not list them.

### DMA requests

Each `METADATA.peripherals` entry has a `dma_requests` list. Each item has a `signal`, a `sourcesel` value and a `sigsel` value. The signal name has no peripheral prefix, for example `RXFL` or `CC0`. A request named like its source gets the signal `GLOBAL`, for example MG26 `LCD`.

- On Series 2, the values come from `<family>_ldmaxbar_defines.h`. Write them to the LDMAXBAR `CH_REQSEL` register.
- On Series 0, the values come from `<family>_dmareq.h`. Write them to the DMA `CH_CTRL` register.

A request source with no peripheral on the chip is not in the data.

`METADATA.dma_channel_count` is the number of DMA channels. The value comes from `LDMA_CH_NUM` on Series 2 and from `DMA_CHAN_COUNT` on Series 0. It is 0 when the chip has no DMA.

### `memory-x` feature

The `memory-x` feature gives the linker a `memory.x` file for the chip. The file has the first flash region and the first RAM region. Do not enable the feature when your application has its own `memory.x`.

## Quick guide

### How to regenerate everything

- Run `./d download-all`

  > Fetches vendor packs into `silabs-data-source/packs/` (idempotent; verifies sha256).

- Run `./d gen-all`

  > Reads `data/registers/*.yaml` as input and regenerates `build/data/` (per-chip JSON) and `build/silabs-metapac/` (the PAC crate).

### How to cut a release

- Run `./deploy.sh [<dest>]`

  > Runs `./d gen-all`, renders the support-matrix README via the `summary` binary, and rsyncs `build/silabs-metapac/` into `<dest>/silabs-metapac/`. Dest defaults to `../silabs-data-generated`; the library crate's `Cargo.lock` is omitted and `target/` is excluded. Review and commit the dest separately.

### How to bootstrap `data/pins/` from scratch

> Rarely needed. The files are committed and maintained by hand.

- Run `./d extract-pins <pin-tool-dir> "<sdk label>" [<pin-tool-dir> "<sdk label>"]...`

  > Reads the Silicon Labs pin tool (`platform/hwconf_data/pin_tool` in the `pintool.zip` release asset of the SDK) and writes `data/pins/<family>.yaml`. The pin-tool files are under the MSLA. Never copy them, or a part of them, into a repo. The command writes only facts. For each part, the family table filtered by the part's bonded pins must equal the part's own data, or the command stops. On Series 0, every location must also agree with `<family>_af_ports.h` and `<family>_af_pins.h`. On Series 2, a signal with a GPIO route register must be whole ports, or the command stops.

  > The files are maintained by hand. When a file is the same as the output, the command says that it is up to date and writes nothing. When a file differs, the command prints the difference and stops. Add `--force` (`./d extract-pins --force ...`) to overwrite it.

### How to bootstrap `data/registers/` from scratch

> Rarely needed. The baseline is already committed. Re-run only when adding a new chip family or fully re-bootstrapping.

- Run `./d seed`

  > Extracts every peripheral on every chip in `silabs-data-source/families.toml`, applies `transforms/<KIND>.yaml` if present, buckets by `(kind, version, block)`, and writes one `data/registers/<kind>_<version>.yaml` per `(kind, version)` with all its blocks. Instances that only add registers, fields or enum values are merged into their superset, and seed prints each merge. Seed stops on a conflict (a name that moves or changes width), so an inconsistency surfaces instead of being silently merged.

## Data sources

Silicon Labs CMSIS DFP packs:

- SVD per OPN — peripheral base addresses and register maps. **Authoritative for register layout.** Its `<interrupt>` blocks are intentionally **not** consulted (the public SVD omits radio peripheral IRQs — FRC, MODEM, AGC, BUFC, PROTIMER, SYNTH, RAC_*, RFECA*).
- Per-chip CMSIS device header (`Device/SiliconLabs/<FAMILY>/Include/<chip>.h`) — **authoritative for the IRQ table.** Parsed by `silabs-data-gen/src/header.rs` for `<NAME>_IRQn = <N>,` enum members.
- pdsc manifest — chip list, memory map, package info, SVD↔OPN mapping.

No `header_map.yaml` analogue is required, and no CubeDB-style separate XML database either — pdsc is sufficient.

Series 0 and Series 1 packs (EFM32 Gecko, EFM32/EFR32 xG1x) stopped at version 4.4.0. Their SVDs carry no `<peripheral><version>` tag. Their headers define `_SILICON_LABS_32B_SERIES 0` without a config macro (Series 0), and `__NVIC_PRIO_BITS` is 2 or 3 instead of 4.

## Per-register YAML curation policy

For register blocks, YAMLs are initially extracted from SVDs, **manually cleaned up and committed. From this point on, they're manually maintained.** We don't maintain "patches". Fixing mistakes and typos in SVDs is done by editing `data/registers/<kind>_<version>.yaml` directly — not by patching the SVD or by re-running `./d seed`.

Regenerating (`./d gen-all`) reads `data/registers/` as **input** and writes only to `build/`. It never overwrites the curated YAMLs. The `./d seed` command is the sole writer to `data/registers/` and is a manual, infrequent bootstrap operation.

Two payoffs from this policy:

- **Fixing vendor mistakes is trivial.** Edit the YAML, commit. No patch system, no diff dance.
- **Consistency across chips.** Each `(kind, version)` has exactly one canonical YAML, shared by every chip that uses it. A HAL written against `gpio_s2v3` works on every chip that routes to `gpio_s2v3` — that's the whole point.

## Toolchain pipeline

Three stages:

```
[Silicon Labs CDN]                     [silabs-data-source/]              [silabs-data/]

.../cmsis-packs/*.pack    --->         packs/*.pack       <--- input ---  silabs-data-gen
   (vendored, sha256-pinned)           families.toml                        (pdsc + SVDs
                                       packs.sha256                          → per-chip JSON
                                                                             + perimap routing)
                                                                                       |
                                                                                       v
                                                                        build/data/chips/*.json
                                                                                       |
                                                                                       v
                                       data/registers/*.yaml  --- input --- silabs-metapac-gen
                                       transforms/*.yaml      --- input ---  (read curated IR
                                                                              + render Rust)
                                                                                       |
                                                                                       v
                                                                      build/silabs-metapac/
                                                                      Cargo.toml (one feature per OPN)
                                                                      src/registers/<kind>_<v>.rs
                                                                      src/chips/<chip>/{mod.rs, device.x}
```

1. **Source acquisition** — `./d download-all` fetches packs into `silabs-data-source/packs/`.
2. **JSON generation** — `silabs-data-gen gen` parses each chip's pdsc + SVD and emits one JSON per chip into `build/data/chips/`. Per peripheral, the chip JSON records its perimap-routed `(kind, version, block)` triple.
3. **PAC generation** — `silabs-metapac-gen gen` reads the chip JSONs + the committed `data/registers/<kind>_<version>.yaml` + `transforms/<KIND>.yaml` and emits the metapac crate into `build/silabs-metapac/`.

The `seed` subcommand sits outside this normal pipeline. It exists only to (re-)write `data/registers/` from raw SVDs on first bootstrap of a family.

### Unifying versions

A HAL is easiest to write when one peripheral kind has few versions and the same names everywhere. Apply these rules when you curate:

- **Additions only: one version.** Some chips or instances add registers, fields or enum values, and nothing else changes. Keep one superset YAML for the version. Add a note to each added item's description that says which chips have it. Example: EUSART0's low-frequency registers are in `eusart_s2v2.yaml`, noted "EUSART0 only".
- **Changed width: one version, two blocks.** A field is wider on some instances. Keep one version with one block per width. Give the differing fieldsets a suffix. Example: `timer_s2v1.yaml` has `Timer` and `Timer32`, with `regs::Cnt` and `regs::Cnt32`.
- **Moved or renamed registers: separate versions.** The layout differs, so the versions stay apart. Example: `usart_s0v1` and `usart_s2v0`.
- **Same meaning: same name.** A register, field or enum value can mean the same thing on two series. Use the same name on both, and prefer the Series 2 name. Write the reference-manual name in the description, for example "(IFC in the reference manual)". Example: Series 0 `IFC` is `if_clr`, and Series 0 `DOUTSET` is `p_dout_set`.

## Adding support for a new peripheral

- First, make sure you can regenerate the YAMLs following the steps above. You should be able to run `./d seed --registers-yaml-dir tmp/seed` against the current chip set without a conflict. Its output has the same structure as the committed `data/registers/`; only hand-curated names and descriptions differ.
- Run `./d seed --chips '<chip regex>' --candidates-dir tmp/candidates`. When chips disagree on a `(kind, version)`, this writes every distinct extraction to `tmp/candidates/<kind>_<version>/<hash>.yaml` (gitignored), with `index.txt` listing which chip peripherals produced each one.
- Diff the extracted YAMLs against each other. The differences can be one of:
  1. Legitimate differences between families or instances (added registers/fields → new `(kind, version)`).
  2. SVD inconsistencies — same register, different names across chips.
  3. SVD mistakes — yes, there are some.
  4. Missing stuff in SVDs — usually enums or doc descriptions.
- Identify how many actually-different (incompatible) versions of the peripheral exist — they must *not* be merged. Use the rules in "Unifying versions" below. Label them as described in "Version labels" below.
- For each version, pick the "best" extraction (most complete, fewest mistakes, richest doc strings). Copy to `data/registers/<kind>_<version>.yaml`.
- Hand-clean (see "Register cleanup" below).
- Minimise the diff between adjacent versions. If `<kind>_s2v<N+1>.yaml` is missing an enum description that `<kind>_s2v<N>.yaml` has, copy it across.
- Set the block name correctly — strip the `_NS` TrustZone suffix (the canonical block name is `GPIO`, not `GPIO_NS`).
- Add `perimap` entries in `silabs-data-gen/src/perimap.rs` routing the relevant `(chip, peripheral_name, svd_version)` triples to the right `(kind, version, block)`. See "perimap" below.
- Regenerate (`./d gen-all`), check `data/chips/*.json` has the right `block:` field, ensure a successful build for at least one chip per affected family.

> **Commit hygiene.** Separate manual edits to YAMLs and changes resulting from regen in separate commits. This makes review and rebase manageable. Commit subjects are plain — describe the change, not the process. No "Phase X", no agent-speak, no co-author trailers.

## Register cleanup

SVDs have widespread annoyances worth fixing during onboarding. `chiptool`'s transforms are the right tool when the cleanup is repeatable; per-YAML hand-edits are right when it's a one-off vendor bug.

- **Remove "useless prefixes".** If every register in `RNG` is named `RNG_*`, the prefix conveys nothing and should go.
- **Remove "useless enums".** Common culprits:
  - `0=disabled, 1=enabled` on `xxEN` / `xxIE` fields — a one-bit field with the obvious meaning doesn't benefit from an enum.
  - "Write 0/1 to clear" enums on `xxIF` fields.
  - See chiptool's `DeleteEnums`, `DeleteUselessEnums` transforms.
- **Recover register / field arrays** like `FOO0, FOO1, FOO2, FOO3` → `FOO[n]`. See chiptool's `MakeRegisterArray`, `MakeFieldArray`.
- **Run `chiptool fmt`** on each YAML for canonical formatting.

## perimap

`perimap` is a regex-keyed map that decides, for every `(chip_name, peripheral_name, svd_version)` triple, which `(kind, version, block)` it routes to. Defined in `silabs-data-gen/src/perimap.rs`.

```rust
// Example entries:
("EFR32MG24.*:GPIO_NS:3",         ("gpio",   "v3", "GPIO")),
("EFR32MG26.*:GPIO_NS:7",         ("gpio",   "v7", "GPIO")),
// EUSART0 carries the LF sub-block; EUSART1+ don't, but the SVD reports
// the same <version>2</version> for all four. Split via perimap.
("EFR32MG24.*:EUSART0_NS:.*",     ("eusart", "v2_lf", "EUSART")),
("EFR32MG24.*:EUSART[1-9]_NS:.*", ("eusart", "v2",    "EUSART")),
```

First match wins. Entries are explicit so future SVD drift doesn't silently change routing.

`perimap` is also where we split structurally-different peripherals that the SVD `<version>` field accidentally merges, and where we strip the vendor `_NS` suffix from block names. SVD `<version>` is the *default* — perimap overrides it when reality disagrees.

### Version labels

Every version label starts with the chip's series: `s<series>v<N>`, optionally followed by a descriptive suffix. The metapac module names (`timer_s2v1`) and the version cfgs (`timer_s2`, `timer_s2v1`) come from the label, so they look the same on every series.

- Series 2 and later SVDs carry a `<peripheral><version>` tag. The default label is `s<series>v<tag>`, for example `gpio_s2v3`.
- When the SVD gives one tag to blocks that really differ, a perimap row adds a suffix, for example `smu_s2v3_mvp`.

### Unversioned SVDs

Chips whose SVDs have no peripheral versions get their label from `perimap::UNVERSIONED` (chip regex → label). These labels are numbered per series in release order (EFM32GG is `s0v1`). `ENTRIES` rows still win, so a block that differs from the family default is split with an `ENTRIES` row. An unversioned peripheral with no route is a hard error.

## CMU metadata

Each `METADATA` peripheral has a `cmu` entry. `silabs-metapac-gen/src/clocks.rs` makes it from the chip's CMU registers:

- `enable` is the clock-gate bit. The generator finds it by name: the gate field has the name of the peripheral (`CLKEN1.TIMER4`, `HFPERCLKEN0.TIMER0`). `OVERRIDES` lists the names that do not match.
- `kernel_clock` is the clock that drives the peripheral. It is one of these:
  - `Mux`: the peripheral has its own select field (`EUSART0CLKCTRL.CLKSEL`). Each enum variant names a source clock.
  - `Clock`: a fixed clock-tree node (`em01grpaclk`, `pclk`). On Series 2 it comes from `SERIES2_FIXED`, a copy of the peripheral-to-branch table in `CMU_ClockFreqGet` (`platform/emlib/src/em_cmu.c` in the Simplicity SDK). On Series 0 it is the bus of the gate register (`HFPERCLKEN0` gives `hfperclk`).

- `bus_enable` is a second gate that must be on before `enable`, and before any access to the peripheral registers. Several peripherals share it, so a HAL counts its users. It is `None` on Series 2. On Series 0, `SERIES0_BUS_ENABLES` in `clocks.rs` gives it from the reference manual:
  - `HFCORECLKEN0.LE` clocks the bus interface of the Low Energy Peripherals (section 5.3.1): RTC, LETIMER0, LEUART0/1, LCD, LESENSE, PCNT0..2, WDOG and BURTC. WDOG and BURTC have no gate of their own, so `bus_enable` is their only CMU data.
  - `HFCORECLKEN0.USBC` clocks the USB core.
  The generator stops when a peripheral on `LFACLKEN0` or `LFBCLKEN0` has no entry.
- `enable.sync_busy` names the `CMU.SYNCBUSY` field that a write to the gate register must wait for. It is `Some` for the Series 0 LF gates (`lfaclken0`, `lfbclken0`), and it has the name of the register. The rule comes from the IR: a CMU register whose description says "(Async Reg)" must have a `SYNCBUSY` field of the same name, or the generator stops. A `SYNCBUSY` bit clears only when the LF branch of the register has a running clock.
- `prescaler` is a CMU field that divides the kernel clock. It is `Some` only for the Series 0 peripherals on LFACLK or LFBCLK: the field of `LFAPRESC0` or `LFBPRESC0` with the name of the peripheral (`lfapresc0.rtc`). It has its own `sync_busy` (`lfapresc0`). Each variant of its enum is named `Div<N>`, and N is the divider. The raw encoding differs by field: `rtc` and `letimer0` use log2(N), but `lcd` starts at `Div16` = 0. So take N from the enum. The generator stops when a variant is not `Div<N>` with N a power of two. The `Mux` select fields also have `sync_busy`, which is `None` on every chip today.

`METADATA.clocks` lists every clock name in the CMU data of the chip, sorted: each `Clock` name and each source of each `Mux`. A source is the lowercase enum variant name, without `Disabled`. A `rt` suffix is dropped when the enum also has the base name (`Hfxort` is `hfxo`), and a `div<N>` suffix is dropped (`Hclkdiv1024` is `hclk`). A source can be an external pin (`pcnts0`). Clocks that no peripheral uses directly are not in the list: the Series 0 oscillators and HFCLK, and the Series 2 `sysclk`. For example, EFM32GG390F1024 has `hfcoreclk`, `hfperclk`, `lfaclk`, `lfbclk` and `pcnts0`.

Group clocks (EM01GRPACLK, EM23GRPACLK, ...) are fixed clocks in this data. The HAL sets their muxes and calculates their frequencies when it sets up the clock tree.

## Extra peripherals

Some blocks are in the device header but not in the SVD. `data/extra_peripherals.yaml` names them for each family, and `gen` adds them to each chip of the family. The base address comes from `#define <NAME>_BASE` in the chip's device header. `gen` stops when the header has no such define, or when the SVD already has the peripheral. Each one needs a register YAML, written by hand from the family's Zlib header:

- EFM32GG `DEVINFO` (`devinfo_s0v1.yaml`, from `efm32gg_devinfo.h`): factory calibration (HFRCO tuning per band, ADC and DAC calibration) and part data (`PART.PROD_REV`, unique number, memory sizes).
- EFM32GG `ROMTABLE` (`romtable_s0v1.yaml`, from `efm32gg_romtable.h`): the chip family and the chip revision (`PID0.REVMAJOR`, `PID2.REVMINORMSB`, `PID3.REVMINORLSB`).

## Adding a new chip family

1. Add the pack URL + version to `silabs-data-source/families.toml`.
2. `./d download-all` to fetch and sha256-pin it.
3. `./d seed --chips '<chip regex>' --candidates-dir tmp/candidates` to extract every peripheral of the new family. `--chips` limits seeding to the new family, so curated YAMLs of other families are never rewritten. `--candidates-dir` writes every divergent IR to `tmp/candidates/<kind>_<version>/` instead of stopping at the first conflict.
4. For each `(kind, version)` not yet in `data/registers/`, follow the "Adding support for a new peripheral" recipe.
5. Add the required `perimap` entries.
6. Check the kernel clocks. Compare `SERIES2_FIXED` in `clocks.rs` with `CMU_ClockFreqGet` for the new config. Generation stops for Series 2 config 1 (xG21), because its table is different.
7. Regenerate, verify one chip per sub-family compiles.

## Agent-facing rules

Load-bearing rules that future automation should pick up:

1. **`data/registers/` is build input, never overwrite.** The only writer is `./d seed`, and that's a manual bootstrap. `./d gen-all` reads from `data/registers/` and writes only to `build/`.
2. **Commit subjects are plain and descriptive.** No phase tags, no progress markers, no agent-speak, no co-author trailers. `Add gpio_s2v3.yaml`, not `Phase 5.2 done: add gpio`.
3. **Plan files are not committed.** Working plans live on disk but are gitignored (`*-plan.md`, `*.plan.md`, `RESET-PLAN.md`).
4. **The user pushes, not the agent.** Hand off after the work is committed; never `git push`.
5. **Stop on `(kind, version, block)` conflicts.** Seed merges instances that only add registers, fields or enum values, and prints each merge. When a name moves or changes width, seed stops and surfaces it. Resolve via transform / hand-curation / perimap split — never via auto-fingerprint-suffix or silent merge.

## Layout

- `silabs-data-gen/` — Rust binary: pack → per-chip JSON, plus perimap-driven peripheral routing.
- `silabs-metapac-gen/` — Rust binary: JSON + curated YAMLs + chiptool → per-IP register modules + typed-const chip mods.
- `transforms/<KIND>.yaml` — chiptool transforms applied during `./d seed` and `./d gen-all`. Sparse — only kinds that need cleanup have a file.
- `data/registers/<kind>_<version>.yaml` — committed source-of-truth register IR, one per `(kind, version)`. Hand-maintained.
- `data/pins/<family>.yaml` — committed pin facts, one per family. Extracted once from the pin tool, then hand-maintained.
- `d` — shell driver wrapping the most common workflows.
- `build/` — gitignored generated outputs (silabs-metapac, per-chip JSON, pack-extracted dirs).

## Dependencies

- Rust 2024 (workspace).
- [`chiptool`](https://github.com/andresv/chiptool) — a fork of [embassy-rs/chiptool](https://github.com/embassy-rs/chiptool) with Silabs-specific changes on the `silabs` branch.

## License

Code in this repo: MIT OR Apache-2.0. The generated `silabs-metapac` source inherits the dual license. Register descriptions are derived from MSLA-licensed Silicon Labs SVD data — see [`silabs-data-source`](https://github.com/andresv/silabs-data-source) for redistribution rationale.
