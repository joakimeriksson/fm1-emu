//! Windows: the default output in WASAPI exclusive mode, event driven at the device's shortest
//! period (--exclusive). The shared mode the default output (cpal) uses mixes on a 10 ms period
//! with its own buffering; exclusive mode hands the device our frames directly, a few ms each,
//! for the lowest latency the device has. Other programs cannot play on that device meanwhile.

use crate::AudioRing;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use wasapi::{calculate_period_100ns, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

/// A format the device may take: rate, bits stored, bits valid, float.
#[derive(Clone, Copy, Debug)]
struct Fmt {
    rate: usize,
    store: usize,
    valid: usize,
    float: bool,
}

/// The firmware's own rate first (no resampling), then 48 kHz; 16-bit first (what most
/// devices take in exclusive mode), then 24 and 32.
const TRY: [Fmt; 8] = [
    Fmt { rate: 44_100, store: 16, valid: 16, float: false },
    Fmt { rate: 48_000, store: 16, valid: 16, float: false },
    Fmt { rate: 44_100, store: 24, valid: 24, float: false },
    Fmt { rate: 48_000, store: 24, valid: 24, float: false },
    Fmt { rate: 44_100, store: 32, valid: 24, float: false },
    Fmt { rate: 48_000, store: 32, valid: 24, float: false },
    Fmt { rate: 44_100, store: 32, valid: 32, float: true },
    Fmt { rate: 48_000, store: 32, valid: 32, float: true },
];

fn wave(f: Fmt) -> WaveFormat {
    let st = if f.float { SampleType::Float } else { SampleType::Int };
    WaveFormat::new(f.store, f.valid, &st, f.rate, 2, None)
}

/// Open the device and play the ring from a thread of its own. Returns once the stream runs
/// (or with why it could not), with a line saying what was opened.
pub fn start(ring: Arc<AudioRing>) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    std::thread::Builder::new()
        .name("fm1-audio-exclusive".into())
        .spawn(move || {
            crate::high_priority();
            if let Err(e) = run(ring, &tx) {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv().unwrap_or_else(|_| Err("the audio thread stopped".into()))
}

fn run(ring: Arc<AudioRing>, ready: &std::sync::mpsc::Sender<Result<String, String>>) -> Result<(), String> {
    let _ = wasapi::initialize_mta();
    let e = |e: wasapi::WasapiError| e.to_string();
    let device = DeviceEnumerator::new().map_err(e)?.get_default_device(&Direction::Render).map_err(e)?;
    let mut client = device.get_iaudioclient().map_err(e)?;
    // the first format the device takes
    let mut chosen = None;
    for f in TRY {
        if let Ok(w) = client.is_supported_exclusive_with_quirks(&wave(f)) {
            chosen = Some((f, w));
            break;
        }
    }
    let (fmt, wf) = chosen.ok_or("the device takes none of 44.1 / 48 kHz at 16, 24 or 32 bits in exclusive mode")?;
    let (_, min_period) = client.get_device_period().map_err(e)?;
    // the shortest period, its buffer aligned to 128 bytes (Intel HDA devices need that)
    let period = client.calculate_aligned_period_near(min_period, Some(128), &wf).map_err(e)?;
    let mode = StreamMode::EventsExclusive { period_hns: period };
    if let Err(first) = client.initialize_client(&wf, &Direction::Render, &mode) {
        // AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED and kin: the buffer the device would take, again
        let frames = client.get_buffer_size().map_err(|_| format!("exclusive mode: {first}"))?;
        let aligned = calculate_period_100ns(frames as i64, fmt.rate as i64);
        client = device.get_iaudioclient().map_err(e)?;
        client
            .initialize_client(&wf, &Direction::Render, &StreamMode::EventsExclusive { period_hns: aligned })
            .map_err(|e2| format!("exclusive mode: {first}; with an aligned period: {e2}"))?;
    }
    let event = client.set_get_eventhandle().map_err(e)?;
    let render = client.get_audiorenderclient().map_err(e)?;
    let block = wf.get_blockalign() as usize;
    let buffer = client.get_buffer_size().map_err(e)?;

    let step = 44_100.0 / fmt.rate as f64;
    let (mut pos, mut cur, mut next) = (0.0f64, (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let mut data: Vec<u8> = Vec::new();
    let mut fill = |n: usize, data: &mut Vec<u8>| {
        data.clear();
        let mut q = ring.q.lock().unwrap();
        let mut starved = false;
        for _ in 0..n {
            pos += step;
            while pos >= 1.0 {
                pos -= 1.0;
                cur = next;
                next = match (q.pop_front(), q.pop_front()) {
                    (Some(l), Some(r)) => (l as f32 / 32768.0, r as f32 / 32768.0),
                    _ => {
                        starved = true;
                        (0.0, 0.0)
                    }
                };
            }
            let t = pos as f32;
            for s in [cur.0 + (next.0 - cur.0) * t, cur.1 + (next.1 - cur.1) * t] {
                let s = s.clamp(-1.0, 1.0);
                match (fmt.float, fmt.store) {
                    (true, _) => data.extend_from_slice(&s.to_le_bytes()),
                    (false, 16) => data.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes()),
                    (false, 24) => data.extend_from_slice(&((s * 8_388_607.0) as i32).to_le_bytes()[..3]),
                    (false, _) => data.extend_from_slice(&(((s * 8_388_607.0) as i32) << 8).to_le_bytes()),
                }
            }
        }
        drop(q);
        if starved && ring.started.load(Ordering::Relaxed) {
            ring.underruns.fetch_add(1, Ordering::Relaxed);
        }
    };
    // a full buffer before the start, so it starts with sound rather than an underrun
    let n = client.get_available_space_in_frames().map_err(e)? as usize;
    fill(n, &mut data);
    render.write_to_device(n, &data, None).map_err(e)?;
    client.start_stream().map_err(e)?;
    let ms = buffer as f64 * 1000.0 / fmt.rate as f64;
    let _ = ready.send(Ok(format!(
        "audio: WASAPI exclusive, {} Hz {}-bit{}, {} frames a period ({ms:.1} ms)",
        fmt.rate, fmt.valid, if fmt.float { " float" } else { "" }, buffer
    )));
    loop {
        if event.wait_for_event(1000).is_err() {
            let _ = client.stop_stream();
            return Err("exclusive audio: the device stopped answering".into());
        }
        let n = client.get_available_space_in_frames().map_err(e)? as usize;
        if n == 0 {
            continue;
        }
        fill(n, &mut data);
        if data.len() != n * block {
            data.resize(n * block, 0);
        }
        render.write_to_device(n, &data, None).map_err(e)?;
    }
}
