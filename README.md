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

## Quick guide

### How to regenerate everything

- Run `./d download-all`

  > Fetches vendor packs into `silabs-data-source/packs/` (idempotent; verifies sha256).

- Run `./d gen-all`

  > Reads `data/registers/*.yaml` as input and regenerates `build/data/` (per-chip JSON) and `build/silabs-metapac/` (the PAC crate).

### How to bootstrap `data/registers/` from scratch

> Rarely needed. The baseline is already committed. Re-run only when adding a new chip family or fully re-bootstrapping.

- Run `./d seed`

  > Extracts every peripheral on every chip in `silabs-data-source/families.toml`, applies `transforms/<KIND>.yaml` if present, buckets by `(kind, version)`, and writes one `data/registers/<kind>_v<version>.yaml` per bucket. Hash-bails on cross-chip divergence so an inconsistency surfaces instead of being silently merged.

## Data sources

Silicon Labs CMSIS DFP packs:

- SVD per OPN — peripheral base addresses and register maps. **Authoritative for register layout.** Its `<interrupt>` blocks are intentionally **not** consulted (the public SVD omits radio peripheral IRQs — FRC, MODEM, AGC, BUFC, PROTIMER, SYNTH, RAC_*, RFECA*).
- Per-chip CMSIS device header (`Device/SiliconLabs/<FAMILY>/Include/<chip>.h`) — **authoritative for the IRQ table.** Parsed by `silabs-data-gen/src/header.rs` for `<NAME>_IRQn = <N>,` enum members. Mirrors stm32-data's approach (`stm32-data-gen/src/header.rs`), which treats the C header as the sole source of truth for interrupts.
- pdsc manifest — chip list, memory map, package info, SVD↔OPN mapping.

No `header_map.yaml` analogue is required, and no CubeDB-style separate XML database either — pdsc is sufficient.

## Per-register YAML curation policy

For register blocks, YAMLs are initially extracted from SVDs, **manually cleaned up and committed. From this point on, they're manually maintained.** We don't maintain "patches". Fixing mistakes and typos in SVDs is done by editing `data/registers/<kind>_v<version>.yaml` directly — not by patching the SVD or by re-running `./d seed`.

Regenerating (`./d gen-all`) reads `data/registers/` as **input** and writes only to `build/`. It never overwrites the curated YAMLs. The `./d seed` command is the sole writer to `data/registers/` and is a manual, infrequent bootstrap operation.

Two payoffs from this policy:

- **Fixing vendor mistakes is trivial.** Edit the YAML, commit. No patch system, no diff dance.
- **Consistency across chips.** Each `(kind, version)` has exactly one canonical YAML, shared by every chip that uses it. A HAL written against `gpio_v3` works on every chip that routes to `gpio_v3` — that's the whole point.

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
                                                                      src/registers/<kind>_v<v>.rs
                                                                      src/chips/<chip>/{mod.rs, device.x}
```

1. **Source acquisition** — `./d download-all` fetches packs into `silabs-data-source/packs/`.
2. **JSON generation** — `silabs-data-gen gen` parses each chip's pdsc + SVD and emits one JSON per chip into `build/data/chips/`. Per peripheral, the chip JSON records its perimap-routed `(kind, version, block)` triple.
3. **PAC generation** — `silabs-metapac-gen gen` reads the chip JSONs + the committed `data/registers/<kind>_v<version>.yaml` + `transforms/<KIND>.yaml` and emits the metapac crate into `build/silabs-metapac/`.

The `seed` subcommand sits outside this normal pipeline. It exists only to (re-)write `data/registers/` from raw SVDs on first bootstrap of a family.

## Adding support for a new peripheral

(Adapted from stm32-data's recipe.)

- First, make sure you can regenerate the YAMLs following the steps above. You should be able to run `./d seed` against the current chip set and end up with no diff to the committed `data/registers/`.
- Run `./d seed --kind <KIND>`. This outputs one extracted YAML per chip instance into `tmp/<KIND>/` (gitignored).
- Diff the extracted YAMLs against each other. The differences can be one of:
  1. Legitimate differences between families or instances (added registers/fields → new `(kind, version)`).
  2. SVD inconsistencies — same register, different names across chips.
  3. SVD mistakes — yes, there are some.
  4. Missing stuff in SVDs — usually enums or doc descriptions.
- Identify how many actually-different (incompatible) versions of the peripheral exist — they must *not* be merged. Name them `v1`, `v2`, … in order of chip release date where possible.
- For each version, pick the "best" extraction (most complete, fewest mistakes, richest doc strings). Copy to `data/registers/<kind>_v<N>.yaml`.
- Hand-clean (see "Register cleanup" below).
- Minimise the diff between adjacent versions. If `<kind>_v<N+1>.yaml` is missing an enum description that `<kind>_v<N>.yaml` has, copy it across.
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

## Adding a new chip family

1. Add the pack URL + version to `silabs-data-source/families.toml`.
2. `./d download-all` to fetch and sha256-pin it.
3. `./d seed --family <NAME>` to extract every peripheral.
4. For each `(kind, version)` not yet in `data/registers/`, follow the "Adding support for a new peripheral" recipe.
5. Add the required `perimap` entries.
6. Regenerate, verify one chip per sub-family compiles.

## Agent-facing rules

Load-bearing rules that future automation should pick up:

1. **`data/registers/` is build input, never overwrite.** The only writer is `./d seed`, and that's a manual bootstrap. `./d gen-all` reads from `data/registers/` and writes only to `build/`.
2. **Commit subjects are plain and descriptive.** No phase tags, no progress markers, no agent-speak, no co-author trailers. `Add gpio_v3.yaml`, not `Phase 5.2 done: add gpio`.
3. **Plan files are not committed.** Working plans live on disk but are gitignored (`*-plan.md`, `*.plan.md`, `RESET-PLAN.md`).
4. **The user pushes, not the agent.** Hand off after the work is committed; never `git push`.
5. **Hash-bail on `(kind, version)` divergence.** When two chips extract to the same `(kind, version)` but produce different IR, stop and surface it. Resolve via transform / hand-curation / perimap split — never via auto-fingerprint-suffix or silent merge.

## Layout

- `silabs-data-gen/` — Rust binary: pack → per-chip JSON, plus perimap-driven peripheral routing.
- `silabs-metapac-gen/` — Rust binary: JSON + curated YAMLs + chiptool → per-IP register modules + typed-const chip mods.
- `transforms/<KIND>.yaml` — chiptool transforms applied during `./d seed` and `./d gen-all`. Sparse — only kinds that need cleanup have a file.
- `data/registers/<kind>_v<version>.yaml` — committed source-of-truth register IR, one per `(kind, version)`. Hand-maintained.
- `d` — shell driver wrapping the most common workflows.
- `build/` — gitignored generated outputs (silabs-metapac, per-chip JSON, pack-extracted dirs).

## Dependencies

- Rust 2024 (workspace).
- [`chiptool`](https://github.com/andresv/chiptool) — a fork of [embassy-rs/chiptool](https://github.com/embassy-rs/chiptool) with Silabs-specific changes on the `silabs` branch.

## License

Code in this repo: MIT OR Apache-2.0. The generated `silabs-metapac` source inherits the dual license. Register descriptions are derived from MSLA-licensed Silicon Labs SVD data — see [`silabs-data-source`](https://github.com/andresv/silabs-data-source) for redistribution rationale.
