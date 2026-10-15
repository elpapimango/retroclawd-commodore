# retroclawd-commodore

Commodore (C64, C128 and the 1541/1570/1571/1581 drives) emulation for [retroclawd](https://github.com/elpapimango/retroclawd), split out of the main repository.
The machine logic lives here; the retroclawd application links none of it. It loads the
shared libraries this workspace builds (`c64_machine_plugin`, `c128_machine_plugin`) at run time.

## Machines

- `c64` -- 6510 with a VIC-II (line or per-cycle accuracy, `--vic-accuracy`), SID (6581/8580), two CIAs, PAL and NTSC,
  keyboard and joysticks, paddles and 1351 mouse, `.tap`/`.t64` tape and `.prg`/`.p00` programs, `.d64`/`.d71`/`.d81`
  disks (writable; `.g64`, `.x64`, `.m3u` playlists), a large set of `.crt` cartridges, REU, GeoRAM, Swiftlink and
  RR-Net, a printer, KERNAL fast-load traps or a real 1541/1570/1571/1581 on the IEC bus (`--true-drive`).
- `c128` -- 8502 with the MMU, VIC-IIe and the VDC 8563 (40 or 80 columns), the Z80 boot hand-off, C64 mode, the same
  drives with burst mode, `--run` auto-typing and debugger disassembly that follows the running CPU (8502 or Z80).

The ROM files and how to call each machine: `docs/firmware.md`.

## Build

```
cargo build --release --workspace
```

produces, in `target/release/`, one library per machine
(`lib<id>_machine_plugin.so`, `<id>_machine_plugin.dll`, `lib<id>_machine_plugin.dylib`).
Put them in the `plugins/` folder next to the `retroclawd` executable (or point
`RETROCLAWD_MACHINE_PLUGIN_DIRS` at the folder) and run `retroclawd <machine> ...`.

## Shared code

Everything that is not specific to this family -- `machine`, `plugin-api`, the `z80`/`mos6502` cores and the
chips/formats several families use -- comes from
[`retroclawd-crates`](https://github.com/elpapimango/retroclawd-crates) as a git dependency pinned to the commit
`d64444aa6a` (the `git = ..., rev = ...` entries in each crate's `Cargo.toml`). To move to a newer commit, run
`tools/bump_pin.sh` in the application repository (it rewrites the pins, runs the tests and records the result), or
by hand: change the rev in every manifest, run `cargo update`, and check the plugin ABI policy in
`retroclawd-crates/docs/plugin-abi.md` (a plugin must be built with the same ABI major, and a protocol version the
application speaks).

The shared crates come from the public repository `retroclawd-crates`, pinned by commit (`rev = ...` in the
`Cargo.toml` files); `tools/bump_pin.sh` in the host repository moves the pin.

## Layout

The directory layout is the same as in the original repository, because the tests find ROMs and
software relative to `CARGO_MANIFEST_DIR/../../../`.

- `crates/machines/c64`
- `crates/machines/c128`
- `crates/machines/1541`
- `crates/machines/drive-link`
- `crates/plugins/c64-machine-plugin`
- `crates/plugins/c128-machine-plugin`
- `crates/plugin-dlls/c64`
- `crates/plugin-dlls/c128`
- `crates/formats/c64-tape`
- `crates/formats/c64-cartridge`
- `crates/formats/d64`
- `crates/formats/d71`
- `crates/formats/d81`
- `crates/formats/g64`
- `crates/formats/cbm-dos`
- `crates/chips/sid`
- `crates/chips/vic-ii`
- `crates/chips/via-6522`
- `crates/chips/wd1770`
- `crates/chips/cia-6526`
- `crates/chips/acia-6551`
- `crates/chips/cs8900a`
- `crates/chips/vdc-8563`

ROM images and software are never committed: `roms/` and `software/` are git-ignored folders for your
own copies. Tests that need them skip when they are absent.

## Tests

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Long-running validation against the real 1541 DOS is `#[ignore]`d: `cargo test --release -p c64 --test real_1541 -- --ignored`.
The ROM-gated boot tests read the files under `roms/` and skip without them.

## License

Free software under the **GNU General Public License v3.0 or later** (see `LICENSE`). No ROM image or commercial
software is part of this repository or its history; supply your own (see `docs/firmware.md`). Third-party sources
that were read for reference are credited in `CREDITS.md`.

## Contributing / issues

Not accepted right now -- this is a personal work-in-progress project. Pull requests and issues will be closed
unreviewed.
