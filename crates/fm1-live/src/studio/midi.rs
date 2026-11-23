//! Input discovery stays outside the audio thread; disconnect releases held input.
use crate::ctl::{self, Ctl};
use fm1_soc::Action;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::Duration,
};
pub struct Inputs {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Inputs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn release(control: &Ctl, tx: &mpsc::Sender<Action>, mapped: bool) {
    if mapped {
        let mut mapper = control.mapper.lock().unwrap();
        let mut view = control.view.lock().unwrap();
        mapper.release_all(&mut |o| ctl::apply(o, tx, &mut view, control.midi_ch));
    }
    for ch in 0..16 {
        for cc in [64, 123, 120] {
            for b in [0xb0 | ch, cc, 0] {
                let _ = tx.send(Action::Midi { b });
            }
        }
    }
}
pub fn connect(wanted: &str, channel: u8, tx: mpsc::Sender<Action>, control: Arc<Ctl>) -> Inputs {
    let stop = Arc::new(AtomicBool::new(false));
    let mut result = Inputs {
        stop: stop.clone(),
        worker: None,
    };
    if wanted == "none" {
        return result;
    }
    let wanted = wanted.to_lowercase();
    result.worker = Some(std::thread::spawn(move || {
        let mut connections = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            if let Ok(probe) = midir::MidiInput::new("fm1-live discovery") {
                let ports: Vec<_> = probe
                    .ports()
                    .into_iter()
                    .filter_map(|port| {
                        let name = probe.port_name(&port).ok()?;
                        if wanted != "all" && !name.to_lowercase().contains(&wanted) {
                            return None;
                        }
                        Some((port.id(), port, name))
                    })
                    .collect();
                control
                    .inputs
                    .lock()
                    .unwrap()
                    .retain(|name| ports.iter().any(|(_, _, live)| live == name));
                let disconnected: Vec<_> = connections
                    .keys()
                    .filter(|id| !ports.iter().any(|(live, _, _)| live == *id))
                    .cloned()
                    .collect();
                for id in disconnected {
                    if let Some((connection, mapped)) = connections.remove(&id) {
                        // Close first so an in-flight callback cannot re-press after cleanup.
                        drop(connection);
                        release(&control, &tx, mapped);
                    }
                }
                for (id, port, name) in ports {
                    if connections.contains_key(&id) {
                        continue;
                    }
                    let Ok(mut input) = midir::MidiInput::new("fm1-live") else {
                        continue;
                    };
                    input.ignore(midir::Ignore::None);
                    let mapped = control.maps(&name);
                    let (sender, mapping) = (tx.clone(), control.clone());
                    match input.connect(
                        &port,
                        "fm1-live in",
                        move |_, msg, _| {
                            if mapped && !msg.is_empty() && msg[0] != 0xf0 {
                                mapping.feed(msg, &sender);
                                return;
                            }
                            for (i, &b) in msg.iter().enumerate() {
                                let b = if i == 0 && channel != 0 && (0x80..0xf0).contains(&b) {
                                    (b & 0xf0) | (channel - 1)
                                } else {
                                    b
                                };
                                let _ = sender.send(Action::Midi { b });
                            }
                        },
                        (),
                    ) {
                        Ok(connection) => {
                            eprintln!("MIDI in: {name}");
                            control.inputs.lock().unwrap().push(name);
                            connections.insert(id, (connection, mapped));
                        }
                        Err(e) => eprintln!("MIDI in: {name}: {e}"),
                    }
                }
            }
            for _ in 0..5 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }));
    result
}
#[cfg(test)]
mod tests {
    #[test]
    fn disconnect_releases_panel_then_sustain_and_notes() {
        let profile = fm1_map::Profile::from_json(include_str!("minilab.json")).unwrap();
        let control = crate::ctl::Ctl::new(Some(profile), 0);
        let (tx, rx) = std::sync::mpsc::channel();
        control.feed(&[0xbf, 21, 127], &tx);
        let _ = rx.recv().unwrap();
        super::release(&control, &tx, true);
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::Key { id: 2, down: false }
        ));
        for ch in 0..16 {
            for cc in [64, 123, 120] {
                for expected in [0xb0 | ch, cc, 0] {
                    assert!(matches!(rx.recv().unwrap(),fm1_soc::Action::Midi{b} if b==expected));
                }
            }
        }
        assert!(!control.view.lock().unwrap().down[2]);
    }
}
