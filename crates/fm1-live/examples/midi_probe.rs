//! List CoreMIDI inputs and monitor incoming bytes for one minute. No MIDI output.
use std::time::{Duration,Instant};
fn main() {
 let probe=midir::MidiInput::new("FM-1 MIDI diagnostic").expect("MIDI initialization");
 let mut connections=Vec::new();
 for port in probe.ports() {
  let name=probe.port_name(&port).unwrap_or_default();
  println!("INPUT: {name}");
  if !name.to_lowercase().contains("minilab") {continue;}
  let mut input=midir::MidiInput::new("FM-1 MIDI diagnostic").unwrap();
  input.ignore(midir::Ignore::None);
  let label=name.clone();
  match input.connect(&port,"diagnostic",move |_,bytes,_|println!("{label}: {bytes:02x?}"),()) {
   Ok(connection)=>connections.push(connection),Err(error)=>eprintln!("{name}: {error}")
  }
 }
 println!("Monitoring {} MiniLab input(s) for 60 seconds; play keys now.",connections.len());
 let start=Instant::now();while start.elapsed()<Duration::from_secs(60) {std::thread::sleep(Duration::from_millis(100));}
}
