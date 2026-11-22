//! Bounded desktop playback. Firmware execution and recording never run on the device callback.
use rtrb::Consumer;
use std::time::{Duration, Instant};

pub const RATE: f64 = 44_100.0;
const FADE: usize = 32;

/// Consumer-owned interpolation and dropout recovery; no locks or allocation while rendering.
pub struct Playback {
    input: Consumer<[f32; 2]>,
    step: f64,
    phase: f64,
    current: [f32; 2],
    next: [f32; 2],
    last: [f32; 2],
    playing: bool,
    recovering: bool,
    fade: usize,
    pub underruns: u64,
    consumed: u64,
}

impl Playback {
    pub fn new(input: Consumer<[f32; 2]>, output_rate: f64) -> Self {
        assert!(output_rate.is_finite() && output_rate > 0.0);
        assert!(input.buffer().capacity() >= 2);
        Self { input, step: RATE / output_rate, phase: 0.0, current: [0.0; 2],
            next: [0.0; 2], last: [0.0; 2], playing: false, recovering: false,
            fade: 0, underruns: 0, consumed: 0 }
    }

    /// The next output position in source frames, excluding interpolation lookahead.
    pub fn source_frame(&self) -> u64 {
        self.consumed.saturating_sub(2) + (self.phase + 1e-9) as u64
    }

    pub fn set_output_rate(&mut self, output_rate: f64) {
        assert!(output_rate.is_finite() && output_rate > 0.0);
        self.step = RATE / output_rate;
    }

    /// `prefill` is in source frames and is checked only on startup or after a dropout.
    pub fn frame(&mut self, prefill: usize) -> [f32; 2] {
        if !self.playing {
            if self.fade > 0 { return self.silence(); }
            if self.input.slots() < prefill.max(2).min(self.input.buffer().capacity()) {
                return self.silence();
            }
            self.current = self.input.pop().unwrap();
            self.next = self.input.pop().unwrap();
            self.consumed += 2;
            self.phase = 0.0;
            self.playing = true;
            self.fade = if self.recovering { FADE } else { 0 };
        }
        while self.phase >= 1.0 {
            match self.input.pop() {
                Ok(frame) => {
                    self.current = self.next;
                    self.next = frame;
                    self.consumed += 1;
                    self.phase -= 1.0;
                }
                Err(_) => {
                    self.playing = false;
                    self.recovering = true;
                    self.fade = FADE;
                    self.underruns += 1;
                    return self.silence();
                }
            }
        }
        let t = self.phase as f32;
        let gain = if self.fade > 0 {
            self.fade -= 1;
            (FADE - self.fade) as f32 / FADE as f32
        } else { 1.0 };
        let frame = std::array::from_fn(|c| (self.current[c] + (self.next[c] - self.current[c]) * t) * gain);
        self.last = frame;
        self.phase += self.step;
        frame
    }

    fn silence(&mut self) -> [f32; 2] {
        if self.fade > 0 {
            let gain = (self.fade - 1) as f32 / self.fade as f32;
            self.fade -= 1;
            self.last = self.last.map(|s| s * gain);
        } else { self.last = [0.0; 2]; }
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefill_preserves_frames_and_stereo_order() {
        let (mut tx, rx) = rtrb::RingBuffer::new(16);
        let mut p = Playback::new(rx, RATE);
        tx.push([0.25, -0.25]).unwrap();
        assert_eq!(p.frame(4), [0.0; 2]);
        for n in 2..=8 { tx.push([n as f32 / 4.0, -n as f32 / 4.0]).unwrap(); }
        for n in 1..=7 { assert_eq!(p.frame(4), [n as f32 / 4.0, -n as f32 / 4.0]); }
        assert_eq!(p.underruns, 0);
    }
    #[test]
    fn a_dropout_fades_to_zero_and_waits_for_refill_once() {
        let (mut tx, rx) = rtrb::RingBuffer::new(128);
        let mut p = Playback::new(rx, RATE);
        for _ in 0..4 { tx.push([0.5, -0.5]).unwrap(); }
        for _ in 0..3 { assert_eq!(p.frame(4), [0.5, -0.5]); }
        let mut last = 0.5;
        for _ in 0..64 {
            let f = p.frame(4);
            assert!(f[0] <= last && f[0] >= 0.0);
            assert_eq!(f[0], -f[1]);
            last = f[0];
        }
        assert_eq!(last, 0.0);
        assert_eq!(p.underruns, 1);
        for _ in 0..3 { tx.push([0.5, -0.5]).unwrap(); }
        assert_eq!(p.frame(4), [0.0; 2]);
        for _ in 0..64 { tx.push([0.5, -0.5]).unwrap(); }
        let first = p.frame(4)[0];
        assert!(first > 0.0 && first < 0.02);
        for _ in 0..32 { p.frame(4); }
        assert_eq!(p.frame(4), [0.5, -0.5]);
        assert_eq!(p.underruns, 1);
    }
    #[test]
    fn rate_conversion_keeps_fractional_phase_between_calls() {
        for rate in [44_100.0, 48_000.0, 96_000.0, 32_000.0] {
            let (mut tx, rx) = rtrb::RingBuffer::new(2048);
            for i in 0..2048 { tx.push([i as f32 / 2048.0; 2]).unwrap(); }
            let mut p = Playback::new(rx, rate);
            for i in 0..1000 {
                let expected = i as f64 * RATE / rate / 2048.0;
                assert!((p.frame(2)[0] as f64 - expected).abs() < 1e-6);
                assert_eq!(p.source_frame(), ((i + 1) as f64 * RATE / rate + 1e-9).floor() as u64);
            }
            assert_eq!(p.underruns, 0);
        }
    }
}

/// Match the standalone producer's scheduling class without claiming a realtime deadline.
pub fn prioritize_producer() {
    #[cfg(target_os = "macos")]
    {
        extern "C" { fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32; }
        // SAFETY: changes only the calling worker's QoS; there are no borrowed pointers.
        unsafe { pthread_set_qos_class_self_np(0x21, 0); }
    }
}

/// Once the firmware streams audio, the emulator runs whenever less than the target of sound
/// is queued: the audio device sets the pace. The target starts at --latency and grows by this
/// much at each underrun, up to AUDIO_TARGET_MAX_MS: a played note waits about that long.
const AUDIO_TARGET_STEP_MS: f64 = 4.0;
const AUDIO_TARGET_MAX_MS: f64 = 80.0;
/// After this long without an underrun the target comes back down (see [`AudioTarget`]).
const AUDIO_TARGET_CALM: Duration = Duration::from_secs(10);

/// How much audio the emulator keeps queued ahead of the device. An underrun raises it by a
/// step. After a calm while it comes back down by a quarter of the way to the lowest it may
/// go (at least half a step), so a burst of underruns (the start, while the JIT compiles the
/// firmware's code, or another program busy for a moment) does not leave the latency high:
/// from 80 ms to 12 in about two minutes. A step down that underruns within the calm while
/// shows the device needs the level before it, which then becomes the lowest it goes for the
/// rest of the run: no probing down and clicking again every minute.
pub struct AudioTarget {
    pub ms: f64,
    /// --latency: the lowest asked for
    floor: f64,
    /// the lowest it goes now: --latency, or a level a step down from which underran
    low: f64,
    calm_since: Instant,
    /// the last step down: the level before it, and when
    probe: Option<(f64, Instant)>,
}

impl AudioTarget {
    pub fn new(floor: f64, now: Instant) -> AudioTarget {
        AudioTarget { ms: floor, floor, low: floor, calm_since: now, probe: None }
    }

    /// One look at the device: `underran` since the last.
    pub fn update(&mut self, underran: bool, now: Instant) {
        if underran {
            if let Some((before, at)) = self.probe.take() {
                if now.duration_since(at) < AUDIO_TARGET_CALM {
                    self.low = self.low.max(before);
                }
            }
            self.calm_since = now;
            self.ms = (self.ms + AUDIO_TARGET_STEP_MS).min(AUDIO_TARGET_MAX_MS.max(self.floor)).max(self.low);
        } else if self.ms > self.low && now.duration_since(self.calm_since) >= AUDIO_TARGET_CALM {
            let down = ((self.ms - self.low) / 4.0).max(AUDIO_TARGET_STEP_MS / 2.0);
            self.probe = Some((self.ms, now));
            self.calm_since = now;
            self.ms = (self.ms - down).max(self.low);
        }
    }
}
