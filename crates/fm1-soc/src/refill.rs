//! The audio halves a firmware leaves unfilled. The DMA plays its buffer in two halves and the
//! firmware refills one while the other plays (`Soc::advance` takes each as it finishes). A
//! firmware too slow for the CPU clock misses some, and its own sound breaks up whatever the
//! host: the DMA takes a half that still holds what it played last time, or, from a firmware that
//! clears a half once it has played, every other half silent. Counted in emulated time, so a host
//! that drops audio does not count. Reading the halves changes nothing in the emulation.

/// Flat and sounding halves alternating over this many halves: a drum machine's gaps between hits
/// alternate over a few halves at most, a starving firmware's for as long as it starves. Raise it
/// if a firmware that plays clean at 240 MHz counts starved halves.
const ALTERNATING: u32 = 11;
/// Those halves in `Refill::sounding`, sounding, flat, sounding... the newest sounding.
const PATTERN: u32 = 0x5555_5555 & ((1 << ALTERNATING) - 1);
/// A half is flat within this peak-to-peak on each channel, four steps of the 16-bit output: a
/// fading tail can repeat exactly or flicker in its last bits.
const FLAT: i64 = 4 << 8;

#[derive(Default)]
pub struct Refill {
    /// each half's address and what it played last (empty: not played yet)
    last: [(u32, Vec<u32>); 2],
    /// the slot of `last` the next new address takes
    older: usize,
    /// the halves so far, the newest in bit 0: 1 sounding, 0 flat
    sounding: u32,
    seen: u32,
    /// halves the firmware did not refill in time
    pub starved: u64,
    /// the half `Soc::advance` reads, kept so that a half allocates nothing
    pub buf: Vec<u32>,
}

impl Refill {
    /// A half the DMA took to play: its address and its words.
    pub fn half(&mut self, adr: u32, words: &[u32]) {
        let flat = is_flat(words);
        self.sounding = self.sounding << 1 | !flat as u32;
        self.seen = (self.seen + 1).min(ALTERNATING);
        let i = match self.last.iter().position(|(a, played)| *a == adr && !played.is_empty()) {
            Some(i) => i,
            None => {
                let i = self.older;
                self.older ^= 1;
                self.last[i] = (adr, Vec::new());
                i
            }
        };
        let played = &mut self.last[i].1;
        if !flat && played == words
            || self.seen == ALTERNATING && self.sounding & ((1 << ALTERNATING) - 1) == PATTERN
        {
            self.starved += 1;
        }
        played.clear();
        played.extend_from_slice(words);
    }

    /// A chip reset: the halves start over (RAM keeps the last ones, which a restarting DMA
    /// would take before the firmware refills them), the count goes on.
    pub fn chip_reset(&mut self) {
        *self = Refill { starved: self.starved, ..Refill::default() };
    }
}

/// The words alternate left and right.
fn is_flat(words: &[u32]) -> bool {
    (0..2).all(|ch| {
        let channel = words.iter().skip(ch).step_by(2).map(|&w| w as i32 as i64);
        channel.clone().max().unwrap_or(0) - channel.min().unwrap_or(0) <= FLAT
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(phase: u32) -> Vec<u32> {
        (0..256).map(|i| ((i * 37 + phase) % 1000) << 8).collect()
    }

    #[test]
    fn a_refilled_half_is_not_counted() {
        let mut r = Refill::default();
        for n in 0..100 {
            r.half(0x1000 + 1024 * (n % 2), &tone(n));
        }
        assert_eq!(r.starved, 0);
    }

    #[test]
    fn a_half_played_again_unchanged_is_counted() {
        let mut r = Refill::default();
        r.half(0x1000, &tone(1));
        r.half(0x1400, &tone(2));
        r.half(0x1000, &tone(1));
        assert_eq!(r.starved, 1);
        r.half(0x1400, &tone(3));
        assert_eq!(r.starved, 1);
    }

    #[test]
    fn silence_played_again_is_not_counted() {
        let mut r = Refill::default();
        for n in 0..10 {
            r.half(0x1000 + 1024 * (n % 2), &[0; 256]);
        }
        assert_eq!(r.starved, 0);
    }

    #[test]
    fn a_silence_at_its_own_level_on_each_side_played_again_is_not_counted() {
        let mut r = Refill::default();
        let levels: Vec<u32> = (0..256).map(|i| if i % 2 == 0 { 5 << 16 } else { (-3i32 << 16) as u32 }).collect();
        for n in 0..10 {
            r.half(0x1000 + 1024 * (n % 2), &levels);
        }
        assert_eq!(r.starved, 0);
    }

    #[test]
    fn a_tail_fading_in_its_last_bits_is_not_counted() {
        let mut r = Refill::default();
        let tail: Vec<u32> = (0..256).map(|i| if i % 4 < 2 { 0 } else { -256i32 as u32 }).collect();
        for n in 0..30 {
            r.half(0x1000 + 1024 * (n % 2), &if n % 2 == 0 { tail.clone() } else { vec![0; 256] });
        }
        assert_eq!(r.starved, 0);
    }

    #[test]
    fn every_other_half_silent_is_counted_once_it_lasts() {
        let mut r = Refill::default();
        for n in 0..9 {
            r.half(0x1000 + 1024 * (n % 2), &if n % 2 == 0 { tone(n) } else { vec![0; 256] });
        }
        assert_eq!(r.starved, 0, "a drum machine's gaps alternate a few times");
        for n in 9..21 {
            r.half(0x1000 + 1024 * (n % 2), &if n % 2 == 0 { tone(n) } else { vec![0; 256] });
        }
        assert_eq!(r.starved, 6, "one for each sounding half once 11 halves alternate");
    }

    #[test]
    fn a_chip_reset_forgets_the_halves_and_keeps_the_count() {
        let mut r = Refill::default();
        r.half(0x1000, &tone(1));
        r.half(0x1000, &tone(1));
        assert_eq!(r.starved, 1);
        r.chip_reset();
        r.half(0x1000, &tone(1));
        assert_eq!(r.starved, 1);
    }
}
