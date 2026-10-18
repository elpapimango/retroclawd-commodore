//! PAL/NTSC selection against the real KERNAL, which detects the video
//! standard itself at boot by timing the raster and records it at `$02A6`
//! (`1` = PAL, `0` = NTSC). Needs `roms/c64/`; skipped when absent.

use c64::C64;
use machine::{Machine, Region};

const PALNTSC: u16 = 0x02A6;
/// Row 1 of screen RAM holds the BASIC banner; `READY.` appears below it.
const SCREEN: u16 = 0x0400;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c64/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn boot(region: Region) -> Option<C64> {
    let mut c64 = C64::new(
        rom("kernal-901227-03.bin")?,
        rom("basic-901226-01.bin")?,
        rom("chargen-901225-01.bin")?,
    )
    .ok()?;
    c64.set_region(region);
    let mut spent = 0u64;
    while spent < 3_200_000 {
        spent += u64::from(c64.step());
    }
    Some(c64)
}

fn screen_has_ready(c64: &C64) -> bool {
    // "READY." in screen codes.
    let ready = [0x12, 0x05, 0x01, 0x04, 0x19, 0x2E];
    (0..1000u16).any(|i| {
        ready
            .iter()
            .enumerate()
            .all(|(k, &b)| c64.read_memory((SCREEN + i + k as u16).into()) == b)
    })
}

#[test]
fn the_kernal_detects_the_selected_standard_and_boots_to_ready() {
    let (Some(pal), Some(ntsc)) = (boot(Region::Pal), boot(Region::Ntsc)) else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    assert_eq!(pal.read_memory(PALNTSC.into()), 1, "PAL");
    assert_eq!(ntsc.read_memory(PALNTSC.into()), 0, "NTSC");
    assert!(screen_has_ready(&pal), "PAL boot reached READY.");
    assert!(screen_has_ready(&ntsc), "NTSC boot reached READY.");
    assert_eq!((pal.clock_hz(), pal.frame_hz()), (985_248, 50));
    assert_eq!((ntsc.clock_hz(), ntsc.frame_hz()), (1_022_727, 60));
}
