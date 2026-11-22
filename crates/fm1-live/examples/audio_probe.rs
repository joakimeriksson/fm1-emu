//! Headless device playback with original firmware; no MIDI, session recall or flash writes.
//! cargo run --release -p fm1-live --example audio_probe -- path/to/firmware 30
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fm1_core::Cpu;
use fm1_host::audio::{AudioTarget, Playback, RATE};
use fm1_soc::{machine, Action, Soc};
use std::sync::{atomic::{AtomicU64, AtomicUsize, Ordering}, Arc};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let path = args.get(1).ok_or("usage: audio_probe FIRMWARE [SECONDS] [QUEUE_MS]")?;
    let seconds: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(30);
    let queue_ms: f64 = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(12.0);
    if !queue_ms.is_finite() || !(1.0..=500.0).contains(&queue_ms) { return Err("QUEUE_MS must be 1..500".into()); }
    let firmware = fm1_soc::firmware_from_bytes(&std::fs::read(path)?, None)?;
    let mut soc = Soc::new(1 << 20, firmware.xip.clone());
    soc.periph.hz = 240_000_000;
    soc.periph.skip_idle = true;
    let mut cpu = Cpu::new(firmware.entry);
    cpu.jit_on = true;
    cpu.user = true;
    let params = 0x01c0_0000 + 0x7fc00;
    use fm1_core::Bus;
    soc.write32(params, params + 0x40);
    fm1_soc::boot_firmware(&mut soc, &mut cpu, &firmware);
    for (ms, id) in [(4000, 21), (4500, 25), (5000, 28)] {
        soc.periph.script.push_back((soc.periph.cycle_of_ms(ms), Action::Key { id, down: true }));
    }
    let device = cpal::default_host().default_output_device().ok_or("no output device")?;
    let supported = device.default_output_config()?;
    if supported.sample_format() != cpal::SampleFormat::F32 { return Err("requires f32 output".into()); }
    let mut config = supported.config();
    config.buffer_size = cpal::BufferSize::Fixed(128);
    let channels = config.channels as usize;
    let (mut producer, consumer) = rtrb::RingBuffer::new(65_536);
    let mut playback = Playback::new(consumer, config.sample_rate.0 as f64);
    let underruns = Arc::new(AtomicU64::new(0));
    let faults = Arc::new(AtomicU64::new(0));
    let count = underruns.clone();
    let errors = faults.clone();
    let prefill = (RATE * queue_ms / 1000.0).ceil() as usize;
    let target_frames = Arc::new(AtomicUsize::new(prefill));
    let callback_target = target_frames.clone();
    let stream = device.build_output_stream(&config, move |out: &mut [f32], _| {
        for frame in out.chunks_mut(channels) {
            let stereo = playback.frame(callback_target.load(Ordering::Relaxed));
            for (c, sample) in frame.iter_mut().enumerate() { *sample = stereo[c % 2]; }
        }
        count.store(playback.underruns, Ordering::Relaxed);
    }, move |_| { errors.fetch_add(1, Ordering::Relaxed); }, None)?;
    println!("device={}, rate={}, buffer=128, queue={queue_ms}ms", device.name()?, config.sample_rate.0);
    stream.play()?;
    fm1_host::audio::prioritize_producer();
    let start = Instant::now();
    let mut cpu1 = None;
    let mut pending_tick = false;
    let mut frames = 0;
    let mut peak = 0i32;
    let mut worst = Duration::ZERO;
    let mut steady_worst = Duration::ZERO;
    let mut warmup_underruns = 0;
    let mut seen = 0;
    let mut target = AudioTarget::new(queue_ms, start);
    while start.elapsed() < Duration::from_secs(seconds) {
        let missed = underruns.load(Ordering::Relaxed);
        target.update(missed > seen, Instant::now());
        seen = missed;
        let prefill = (target.ms * RATE / 1000.0).ceil() as usize;
        target_frames.store(prefill, Ordering::Relaxed);
        if start.elapsed() < Duration::from_secs(10) { warmup_underruns = missed; }
        if producer.buffer().capacity() - producer.slots() >= prefill {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let run = Instant::now();
        machine::run_dual(&mut cpu1, &mut soc, 20_000, 1 << 20, true,
            |soc, n| machine::run_blocks(&mut cpu, soc, n, &mut pending_tick))?;
        let elapsed = run.elapsed();
        worst = worst.max(elapsed);
        if start.elapsed() >= Duration::from_secs(10) { steady_worst = steady_worst.max(elapsed); }
        for frame in soc.periph.audio.chunks_exact(2) {
            peak = peak.max((frame[0] as i32).abs()).max((frame[1] as i32).abs());
            producer.push([frame[0] as f32 / 32768.0, frame[1] as f32 / 32768.0])?;
            frames += 1;
        }
        soc.periph.audio.clear();
    }
    println!("seconds={seconds}, frames={frames}, peak={peak}, underruns={}, device_errors={}, worst_run_ms={:.3}",
        underruns.load(Ordering::Relaxed), faults.load(Ordering::Relaxed), worst.as_secs_f64()*1000.0);
    println!("final_queue_ms={:.1}, underruns_after_10s={}, steady_worst_run_ms={:.3}", target.ms, seen.saturating_sub(warmup_underruns), steady_worst.as_secs_f64()*1000.0);
    if faults.load(Ordering::Relaxed) > 0 { return Err("audio device error".into()); }
    Ok(())
}
