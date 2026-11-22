//! A 44.1 kHz 16-bit stereo WAV recorder (fm1-live's F3, the plugin's Record).

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The most frames a WAV holds: its sizes are 32-bit (about 6.7 hours at 44.1 kHz).
const MAX_FRAMES: u64 = (u32::MAX as u64 - 36) / 4;
/// How often the header's lengths are brought up to date while recording (2 s of audio), so a
/// crash or a killed host leaves a file that plays up to there.
const PATCH_FRAMES: u64 = 2 * 44_100;

/// A 44.1 kHz 16-bit stereo WAV being written.
pub struct Wav {
    file: std::io::BufWriter<std::fs::File>,
    pub path: PathBuf,
    pub frames: u64,
    patched: u64,
    /// Why the recording stopped taking samples (a failed write, the size limit); `finish` reports it.
    pub error: Option<String>,
}

impl Wav {
    /// A new file in the user's Music folder (fm1-emu/), else the temporary folder's fm1-emu/.
    pub fn create(name: &str) -> std::io::Result<Wav> {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|h| PathBuf::from(h).join("Music").join("fm1-emu"));
        let dir = home
            .into_iter()
            .chain(std::iter::once(std::env::temp_dir().join("fm1-emu")))
            .find(|d| std::fs::create_dir_all(d).is_ok())
            .ok_or_else(|| std::io::Error::other("no folder to record into (Music/fm1-emu, nor the temporary folder)"))?;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        Self::create_in(&dir, &format!("fm1-{}-{secs}", clean.trim_matches('-')))
    }

    /// A new file `<stem>.wav` in `dir`, or `<stem>-2.wav`, `-3`, ... : never one that exists.
    pub fn create_in(dir: &Path, stem: &str) -> std::io::Result<Wav> {
        let mut n = 1;
        let (path, file) = loop {
            let path = dir.join(if n == 1 { format!("{stem}.wav") } else { format!("{stem}-{n}.wav") });
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => break (path, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                Err(e) => return Err(e),
            }
        };
        let mut file = std::io::BufWriter::new(file);
        file.write_all(&header(0))?;
        Ok(Wav { file, path, frames: 0, patched: 0, error: None })
    }

    /// Interleaved stereo samples. After a failed write or at the size limit it takes no more and
    /// sets `error`; the samples written until then stay playable.
    pub fn write(&mut self, s: &[i16]) {
        if self.error.is_some() {
            return;
        }
        let room = (MAX_FRAMES - self.frames) as usize;
        let s = &s[..s.len().min(room * 2) & !1];
        let mut b = Vec::with_capacity(s.len() * 2);
        for x in s {
            b.extend_from_slice(&x.to_le_bytes());
        }
        if let Err(e) = self.file.write_all(&b) {
            self.error = Some(format!("writing stopped: {e}"));
            return;
        }
        self.frames += s.len() as u64 / 2;
        if self.frames >= MAX_FRAMES {
            self.error = Some("stopped at the 4 GB a WAV can hold".into());
        } else if self.frames - self.patched >= PATCH_FRAMES {
            if let Err(e) = self.patch() {
                self.error = Some(format!("writing stopped: {e}"));
            }
        }
    }

    /// The header's lengths to what has been written, and back to the end.
    fn patch(&mut self) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header((self.frames * 4) as u32))?;
        self.file.seek(SeekFrom::End(0))?;
        self.patched = self.frames;
        Ok(())
    }

    /// Close it with its length in the header. An error if the recording stopped early (the file
    /// still has what was written until then).
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        let patched = self.patch().and_then(|_| self.file.flush());
        if let Some(e) = self.error {
            return Err(std::io::Error::other(format!("{}: {e}", self.path.display())));
        }
        patched?;
        Ok(self.path)
    }
}

/// A bounded recording queue isolates filesystem stalls from audio production.
/// If storage cannot keep up, stop the take rather than omit samples inside it.
pub struct AsyncWav {
    pub path: PathBuf,
    pub frames: u64,
    pub error: Option<String>,
    sender: Option<std::sync::mpsc::SyncSender<Vec<i16>>>,
    worker: Option<std::thread::JoinHandle<std::io::Result<PathBuf>>>,
}

impl AsyncWav {
    pub fn create(name: &str) -> std::io::Result<Self> {
        Self::start(Wav::create(name)?)
    }

    fn start(mut wav: Wav) -> std::io::Result<Self> {
        let path = wav.path.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<i16>>(32);
        let worker = std::thread::Builder::new().name("fm1-recording".into()).spawn(move || {
            for samples in receiver {
                wav.write(&samples);
                if wav.error.is_some() { break; }
            }
            wav.finish()
        })?;
        Ok(Self { path, frames: 0, error: None, sender: Some(sender), worker: Some(worker) })
    }

    pub fn write(&mut self, samples: &[i16]) {
        if self.error.is_some() { return; }
        match self.sender.as_ref().unwrap().try_send(samples.to_vec()) {
            Ok(()) => self.frames += samples.len() as u64 / 2,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.error = Some("recording stopped: storage could not keep up".into());
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.error = Some("recording stopped: file writer failed".into());
            }
        }
    }

    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        self.sender.take();
        let result = self.worker.take().unwrap().join()
            .map_err(|_| std::io::Error::other("recording thread stopped"))?;
        // A disk error is more specific than the disconnected-queue error.
        let path = result?;
        if let Some(error) = self.error.take() { return Err(std::io::Error::other(error)); }
        Ok(path)
    }
}

impl Drop for AsyncWav {
    fn drop(&mut self) {
        // The worker finalizes the accepted samples even if the host drops a take.
        self.sender.take();
    }
}

fn header(data: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    let (rate, ch, bits) = (44_100u32, 2u16, 16u16);
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&ch.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * ch as u32 * bits as u32 / 8).to_le_bytes());
    h[32..34].copy_from_slice(&(ch * bits / 8).to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fm1-wav-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn data_len(p: &Path) -> u32 {
        let b = std::fs::read(p).unwrap();
        u32::from_le_bytes(b[40..44].try_into().unwrap())
    }

    #[test]
    fn a_second_take_with_the_same_name_does_not_overwrite_the_first() {
        let d = dir("names");
        let mut a = Wav::create_in(&d, "fm1-x-1").unwrap();
        a.write(&[1, 2, 3, 4]);
        let a = a.finish().unwrap();
        let b = Wav::create_in(&d, "fm1-x-1").unwrap().finish().unwrap();
        assert_ne!(a, b);
        assert_eq!(b.file_name().unwrap(), "fm1-x-1-2.wav");
        assert_eq!(data_len(&a), 8, "the first take keeps its samples");
        assert_eq!(std::fs::read(&a).unwrap().len(), 44 + 8);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_header_follows_the_recording_while_it_runs() {
        let d = dir("patch");
        let mut w = Wav::create_in(&d, "t").unwrap();
        let block = vec![0i16; 2 * 1000];
        for _ in 0..(PATCH_FRAMES / 1000 + 1) {
            w.write(&block);
        }
        // not finished (as after a crash): the header already counts the first 2 s
        let len = data_len(&w.path);
        assert!(len as u64 >= PATCH_FRAMES * 4 && len as u64 <= w.frames * 4, "{len}");
        let p = w.finish().unwrap();
        assert_eq!(data_len(&p) as u64, (PATCH_FRAMES / 1000 + 1) * 1000 * 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stopped_recording_says_so_when_finished() {
        let d = dir("stop");
        let mut w = Wav::create_in(&d, "t").unwrap();
        w.write(&[5, 6]);
        w.error = Some("writing stopped: disk full".into());
        w.write(&[7, 8]);
        assert_eq!(w.frames, 1, "nothing taken after the error");
        let e = w.finish().unwrap_err().to_string();
        assert!(e.contains("disk full"), "{e}");
        assert_eq!(data_len(&d.join("t.wav")), 4, "what was written before stays playable");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    #[test]
    fn finishing_drains_and_finalizes_every_accepted_stereo_frame() {
        let dir = std::env::temp_dir().join(format!("fm1-async-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut recording = AsyncWav::start(Wav::create_in(&dir, "take").unwrap()).unwrap();
        recording.write(&[100, -100, 200, -200]);
        recording.write(&[300, -300]);
        assert_eq!(recording.frames, 3);
        let path = recording.finish().unwrap();
        let data = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_le_bytes(data[40..44].try_into().unwrap()), 12);
        let expected: Vec<u8> = [100i16, -100, 200, -200, 300, -300].into_iter().flat_map(i16::to_le_bytes).collect();
        assert_eq!(&data[44..], expected);
        std::fs::remove_file(path).unwrap();
        let _ = std::fs::remove_dir(dir);
    }
    #[test]
    fn a_full_recording_queue_stops_the_take_without_blocking() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let mut recording = AsyncWav { path: PathBuf::new(), frames: 0, error: None,
            sender: Some(sender), worker: None };
        recording.write(&[1, -1]);
        recording.write(&[2, -2]);
        recording.write(&[3, -3]);
        assert_eq!(recording.frames, 1);
        assert!(recording.error.as_ref().unwrap().contains("could not keep up"));
    }
}
