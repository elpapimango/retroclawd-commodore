# ROM images (Commodore)

`../firmware.toml` lists the SHA-1 of the dumps each machine option accepts, for `retroclawd --firmware auto` (firmware fetched from a RomM server by hash). It holds hashes only; no ROM is needed for that.

ROM images are copyrighted and are **not** part of this repository. Put your own dumps in a local `roms/` folder
(git-ignored) or let `retroclawd --firmware auto` fetch them by hash. The application never bundles
ROMs; supply your own dumps. The application checks the size of every ROM and refuses a mismatch (a
Spectrum ROM is not silently loaded into a ZX81).

## Commodore 64

`c64` needs three separate images (a real C64 has no single boot ROM) under `c64/`; the known-good set is the
US/EU revision 3 one (other revisions are present too):

| Option | File | Size |
|---|---|---|
| `--rom-kernal` | `c64/kernal-901227-03.bin` | 8K |
| `--rom-basic` | `c64/basic-901226-01.bin` | 8K |
| `--rom-char` | `c64/chargen-901225-01.bin` | 4K |

```
retroclawd c64 --rom-kernal machines/commodore/roms/c64/kernal-901227-03.bin \
  --rom-basic machines/commodore/roms/c64/basic-901226-01.bin \
  --rom-char machines/commodore/roms/c64/chargen-901225-01.bin \
  --headless --max-cycles 2000000 --dump-screen out.png
```

`--max-cycles` matters in a headless run: the KERNAL boot (RAM test + BASIC cold start) needs 1-2 million
instructions before `READY.` appears; with fewer you see only border and background. For `--region ntsc` use the
`-03` KERNAL, which detects the standard itself (the older `-01` does not).

## Commodore 128

`c128` needs five images under `c128/` (the two BASIC halves must be one 32K file). It starts in 40 columns;
`--display 80` holds the 40/80 key down at reset for the VDC screen; `--mode c64` boots C64 mode.

| Option | File | Size |
|---|---|---|
| `--rom-basic` | `basiclo-318018-04.bin` + `basichi-318019-04.bin`, concatenated | 32K |
| `--rom-kernal` | `kernal-318020-05.bin` (editor + Z80 BIOS + kernal) | 16K |
| `--rom-char` | `chargen-390059-01.bin` (C64 set, then C128 set) | 8K |
| `--rom-c64-basic` | `basic64-901226-01.bin` | 8K |
| `--rom-c64-kernal` | `kernal64-901227-03.bin` | 8K |

```
cat machines/commodore/roms/c128/basiclo-318018-04.bin machines/commodore/roms/c128/basichi-318019-04.bin > c128-basic.bin
retroclawd c128 --rom-basic c128-basic.bin \
  --rom-kernal machines/commodore/roms/c128/kernal-318020-05.bin \
  --rom-char machines/commodore/roms/c128/chargen-390059-01.bin \
  --rom-c64-basic machines/commodore/roms/c128/basic64-901226-01.bin \
  --rom-c64-kernal machines/commodore/roms/c128/kernal64-901227-03.bin \
  --headless --max-cycles 3000000 --dump-screen out.png
```

## Drives

A disk needs the instant KERNAL traps (the default) or a real drive: `--true-drive --rom-1541 <dos rom>`. The DOS
ROMs are 16K each: `1541/dos1541-325302-01+901229-05.bin`, `1541/dos1541ii-251968-03.bin`, and in `c64/` the
unnamed copies from the Denise emulator (`VC1541`, `VC1541-II` are identical to those two, `VC1541-C` is the
1541-C ROM, `VC1570`/`VC1571`/`VC1581` serve `--drive-type 1570|1571|1581`; `scpu64`, `Kernal`, `Basic`,
`Char` are not used by any machine yet). The C128's default drive type is the 1571; a `.d81` needs
`--drive-type 1581 --rom-1541 machines/commodore/roms/c64/VC1581`.

Validation: `cargo test --release -p c64 --test real_1541 -- --ignored` boots the real KERNAL and DOS side by side
and loads every `.d64` in `software/c64` over the emulated IEC bus.
