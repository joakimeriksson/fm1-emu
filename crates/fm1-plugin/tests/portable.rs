//! Shared saves boot through the real plugin engine, including same-firmware replacement.
use fm1_plugin::{engine::{self, Cmd, Midi, Shared, View}, savefile::{self, SaveData}, Firmware, Fm1Params, Sector};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

struct Engine {
    shared: Arc<Shared>,
    params: Arc<Fm1Params>,
    tx: mpsc::Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
    _midi: rtrb::Producer<Midi>,
    _audio: rtrb::Consumer<[f32; 2]>,
}
impl Engine {
    fn new() -> Self {
        let shared = Arc::new(Shared {
            view: Mutex::new(View { lcd: vec![0; 240 * 240], leds: [0.0; 41], pads: [0; 16],
                board: fm1_soc::Board::Fm1, booted: false, halted: false, speed: 0.0, starved: 0, late: 0 }),
            quit: Default::default(), underruns: Default::default(),
        });
        let params = Arc::new(Fm1Params::default());
        let (tx, rx) = mpsc::channel();
        let (midi, midi_rx) = rtrb::RingBuffer::new(64);
        let (audio_tx, audio) = rtrb::RingBuffer::new(16384);
        let (sh, p) = (shared.clone(), params.clone());
        let thread = std::thread::spawn(move || engine::run(sh, p, rx, midi_rx, audio_tx));
        Self { shared, params, tx, thread: Some(thread), _midi: midi, _audio: audio }
    }
    fn load(&self, data: &SaveData) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Load { firmware: data.firmware.clone(), sectors: data.sectors.clone(), reply: tx }).unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }
    fn snapshot(&self, firmware: &Firmware) -> mpsc::Receiver<Result<Vec<Sector>, String>> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Snapshot { pkg: firmware.pkg.clone(), reply: tx }).unwrap();
        rx
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.quit.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() { t.join().unwrap(); }
    }
}
fn sample() -> Option<SaveData> {
    if let Ok(path) = std::env::var("FM1_TEST_SAVE_INPUT") {
        return Some(savefile::read(std::path::Path::new(&path)).unwrap());
    }
    let pkg = match std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc")) {
        Ok(pkg) => pkg,
        Err(_) => { eprintln!("missing local Felucca package; run node tools/fetch_firmwares.mjs felucca"); return None; }
    };
    Some(SaveData { firmware: Firmware { name: "Portable Felucca".into(), pkg: Arc::new(pkg) },
        sectors: vec![Sector { at: 0xff000, data: Arc::new(vec![0x56; 4096]) }] })
}
fn byte(sectors: &[Sector]) -> u8 { sectors.iter().find(|s| s.at == 0xff000).unwrap().data[0] }

#[test]
fn portable_save_round_trip_and_same_package_replacement() {
    let Some(mut data) = sample() else { return };
    let engine = Engine::new();
    engine.load(&data).unwrap();
    let sectors = engine.snapshot(&data.firmware).recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(byte(&sectors), 0x56);
    assert_eq!(*engine.params.firmware.lock().unwrap().pkg, *data.firmware.pkg);
    let portable = SaveData { firmware: data.firmware.clone(), sectors };
    let restored = savefile::decode(&savefile::encode(&portable).unwrap()).unwrap();
    assert_eq!(restored.firmware.pkg, data.firmware.pkg);
    assert_eq!(byte(&restored.sectors), 0x56);
    if let Ok(path) = std::env::var("FM1_TEST_SAVE_OUTPUT") {
        savefile::write(std::path::Path::new(&path), &restored).unwrap();
    }

    // Bypass the package-only boot cache: the package is identical, the patches differ.
    data.sectors[0].data = Arc::new(vec![0x73; 4096]);
    engine.load(&data).unwrap();
    assert_eq!(byte(&engine.snapshot(&data.firmware).recv_timeout(Duration::from_secs(10)).unwrap().unwrap()), 0x73);
    assert_eq!(byte(&engine.params.flash.lock().unwrap()), 0x73);
    assert!(engine.shared.view.lock().unwrap().booted);

    // Invalid imports leave the previous setup and its project state intact.
    let mut invalid = data.clone(); invalid.sectors[0].at = 1;
    assert!(engine.load(&invalid).is_err());
    invalid = data.clone(); invalid.firmware.pkg = Arc::new(vec![0; 960]);
    assert!(engine.load(&invalid).is_err());
    assert_eq!(byte(&engine.snapshot(&data.firmware).recv_timeout(Duration::from_secs(10)).unwrap().unwrap()), 0x73);
}

#[test]
fn a_firmware_change_cancels_a_pending_export() {
    let Some(data) = sample() else { return };
    let engine = Engine::new(); engine.load(&data).unwrap();
    let pending = engine.snapshot(&data.firmware);
    engine.load(&data).unwrap();
    assert!(pending.recv_timeout(Duration::from_secs(10)).unwrap().err().unwrap().contains("changed"));
    let wrong = Firmware { name: "wrong".into(), pkg: Arc::new(vec![0; 960]) };
    assert!(engine.snapshot(&wrong).recv_timeout(Duration::from_secs(10)).unwrap().is_err());
}
