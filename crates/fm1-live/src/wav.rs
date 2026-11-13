//! A 44.1 kHz 16-bit stereo WAV recorder (fm1-live's F3, the plugin's Record).

use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

/// A 44.1 kHz 16-bit stereo WAV being written.
pub struct Wav {
    file: std::io::BufWriter<std::fs::File>,
    pub path: PathBuf,
    pub frames: u64,
}

impl Wav {
    /// A new file in the user's Music folder (fm1-emu/), else the current folder.
    pub fn create(name: &str) -> std::io::Result<Wav> {
        let dir = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|h| PathBuf::from(h).join("Music").join("fm1-emu"))
            .filter(|d| std::fs::create_dir_all(d).is_ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let path = dir.join(format!("fm1-{}-{secs}.wav", clean.trim_matches('-')));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        file.write_all(&header(0))?;
        Ok(Wav { file, path, frames: 0 })
    }
    /// Interleaved stereo samples.
    pub fn write(&mut self, s: &[i16]) {
        let mut b = Vec::with_capacity(s.len() * 2);
        for x in s {
            b.extend_from_slice(&x.to_le_bytes());
        }
        if self.file.write_all(&b).is_ok() {
            self.frames += s.len() as u64 / 2;
        }
    }
    /// Close it with its length in the header.
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        self.file.flush()?;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header((self.frames * 4).min(u32::MAX as u64 - 36) as u32))?;
        Ok(self.path)
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
