//! A firmware that halts: the window is told that it stopped, and why. Needs
//! reference/firmwares/felucca.fwsc (tools/fetch_firmwares.mjs).

use fm1_plugin::engine::{self, Cmd, Shared, View};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Wait (at most `secs`) until `what` holds.
fn wait_for(secs: u64, mut what: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if what() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    what()
}

#[test]
fn a_firmware_that_halts_is_shown_as_halted_with_the_reason() {
    let pkg = match std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc")) {
        Ok(p) => Arc::new(p),
        Err(_) => return eprintln!("no reference/firmwares/felucca.fwsc: skipped"),
    };
    let shared = Arc::new(Shared {
        view: Mutex::new(View { lcd: vec![0; 240 * 240], leds: [0.0; 41], pads: [0; 16], board: fm1_soc::Board::Fm1, booted: false, halted: false, speed: 0.0, starved: 0, late: 0 }),
        quit: Default::default(),
    });
    let params = Arc::new(fm1_plugin::Fm1Params::default());
    let (tx, rx) = mpsc::channel();
    let (_midi, midi_rx) = rtrb::RingBuffer::new(64);
    let (audio_tx, audio) = rtrb::RingBuffer::new(16384);
    let (sh, p) = (shared.clone(), params.clone());
    let t = std::thread::spawn(move || engine::run(sh, p, rx, midi_rx, audio_tx));
    tx.send(Cmd::Block(1024)).unwrap();
    tx.send(Cmd::Boot { pkg: pkg.clone(), sectors: Vec::new() }).unwrap();
    // it runs, the window's view kept up to date: flat out until the ring holds its lead (the block and
    // the default lead, 2048 frames), and then, nobody taking its audio, in real time
    let lit = || shared.view.lock().unwrap().lcd.iter().filter(|&&p| p != 0).count();
    assert!(wait_for(10, || lit() > 1000), "the firmware did not start");
    assert!(params.halt.lock().unwrap().is_none());
    assert!(wait_for(10, || audio.slots() >= 2048), "the ring was not filled");
    std::thread::sleep(Duration::from_millis(300)); // (it runs in real time from 50 ms after the last frame taken)

    // the same package with the sector its entry is in overwritten in flash: its first instruction is none
    // (and a sector as the package has it, so what the project holds is not what the machine's flash differs by)
    let same = fm1_soc::stock_from_package(&pkg, None).unwrap().flash[0x5000..0x6000].to_vec();
    tx.send(Cmd::Boot { pkg, sectors: vec![(0x4000, vec![0xff; 4096]), (0x5000, same)] }).unwrap();
    let halted = || shared.view.lock().unwrap().halted;
    assert!(wait_for(5, halted), "the window was not told that the firmware halted");
    let why = params.halt.lock().unwrap().clone().expect("the reason");
    eprintln!("{why}");
    assert!(why.starts_with("The firmware stopped: ") && why.contains("pc 0x02000120"), "{why}");
    // and the view of a halted machine is kept as a running one's is
    shared.view.lock().unwrap().halted = false;
    assert!(wait_for(2, halted), "a halted machine's view is not kept up to date");

    // what its flash holds reaches the project's state after the halt too (the look every 2 s)
    let saved = || params.flash.lock().unwrap().iter().map(|s| s.at).collect::<Vec<_>>();
    assert!(wait_for(4, || saved() == [0x4000]), "the halted machine's flash was not saved: {:x?}", saved());

    shared.quit.store(true, std::sync::atomic::Ordering::Relaxed);
    t.join().unwrap();
}
