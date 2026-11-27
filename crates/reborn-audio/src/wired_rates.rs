//! The wired output's rate for a source: 44.1 or 48 kHz, both from the DAC's crystal.
use std::sync::atomic::{AtomicBool, Ordering};

/// The wired output's default rate, and its only one on kernels without 48 kHz.
pub const WIRED_RATE: u32 = 44_100;
/// The wired output's second rate, for the 48 kHz family.
pub const WIRED_RATE_48K: u32 = 48_000;

/// Set once a wired plan at 48 kHz failed where 44.1 kHz then worked: the kernel offers
/// 44.1 kHz only (Y2Linux before native 48 kHz). Kept for the rest of the run, so a 48 kHz
/// source neither ends a gapless album nor reopens the sink again.
static REFUSED_48K: AtomicBool = AtomicBool::new(false);

/// The rate the wired output plays a source at. Both rates run from the CS43131's
/// 22.5792 MHz crystal, never from its PLL (which knocked the crystal out on some players,
/// #52): the crystal clocks 44.1 kHz by division and 48 kHz through the DAC's serial-port
/// clock generator (DS1155F2 Table 4-6, N/M 20/147; played on a Y2, 2026-11-27). The SoC's
/// I2S has no higher rate. So the 48 kHz family (multiples of 8 kHz: 32, 48, 96, 192 kHz)
/// plays at 48 kHz, everything else (the 44.1 kHz family, odd or unknown rates) at 44.1 kHz,
/// and the decoder converts whatever differs.
pub fn wired_rate(source_rate: u32) -> u32 {
    wired_rate_for(source_rate, !REFUSED_48K.load(Ordering::Relaxed))
}

fn wired_rate_for(source_rate: u32, offers_48k: bool) -> u32 {
    if offers_48k && source_rate != 0 && source_rate.is_multiple_of(8_000) {
        WIRED_RATE_48K
    } else {
        WIRED_RATE
    }
}

/// Record that this kernel refused 48 kHz on the wired output (`REFUSED_48K`).
pub(crate) fn note_48k_refused() {
    REFUSED_48K.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_family_plays_at_its_own_rate() {
        for source in [11_025, 22_050, 44_100, 88_200, 176_400, 352_800] {
            assert_eq!(wired_rate_for(source, true), WIRED_RATE, "{source}");
        }
        for source in [8_000, 16_000, 32_000, 48_000, 96_000, 192_000, 384_000] {
            assert_eq!(wired_rate_for(source, true), WIRED_RATE_48K, "{source}");
        }
        // A rate of neither family, or none known (a station before it plays): the default.
        for source in [0, 37_800, 50_000, 44_000] {
            assert_eq!(wired_rate_for(source, true), WIRED_RATE, "{source}");
        }
    }

    #[test]
    fn a_kernel_without_48k_plays_everything_at_44k1() {
        for source in [44_100, 48_000, 96_000, 0] {
            assert_eq!(wired_rate_for(source, false), WIRED_RATE, "{source}");
        }
    }

    #[test]
    fn both_wired_rates_run_from_the_crystal_without_the_pll() {
        const CRYSTAL: u64 = 22_579_200;
        // 44.1 kHz divides the crystal (64 bit clocks per frame, 1/8 of the crystal).
        assert_eq!(CRYSTAL % u64::from(WIRED_RATE), 0);
        assert_eq!(CRYSTAL / 8, 64 * u64::from(WIRED_RATE));
        // 48 kHz: the serial port's clock generator, N/M = 20/147, exact (DS1155F2 Table 4-6).
        assert_eq!(CRYSTAL * 20 % 147, 0);
        assert_eq!(CRYSTAL * 20 / 147, 64 * u64::from(WIRED_RATE_48K));
    }
}
