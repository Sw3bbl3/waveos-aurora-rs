//! Sound playback: streams of 48 kHz stereo samples, WAV files, the volume.

use crate::abi::{audio as abi, nr};
use crate::sys::call;
use crate::Result;
use alloc::vec::Vec;

pub use abi::RATE;

/// A playback stream, mixed with every other one by the system.
pub struct Stream {
    fd: u64,
}

impl Stream {
    /// Fails with `ENODEV` when there is no sound device.
    pub fn open() -> Result<Stream> {
        call(nr::AUDIO_OPEN, &[]).map(|fd| Stream { fd })
    }

    /// Queues interleaved stereo samples; blocks while about a quarter of a
    /// second is already queued.
    pub fn write(&mut self, samples: &[i16]) -> Result<()> {
        let bytes = unsafe { core::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 2) };
        let mut done = 0;
        while done < bytes.len() {
            done += crate::io::write_fd(self.fd, &bytes[done..])?;
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // What is still queued plays out after the stream is closed.
        let _ = call(nr::CLOSE, &[self.fd]);
    }
}

/// Decodes a WAV file (8/16-bit PCM, mono or stereo, any rate) to the
/// playback format.
pub fn decode_wav(data: &[u8]) -> Option<Vec<i16>> {
    aurora_wav::decode(data, RATE as usize)
}

/// Plays a WAV file to the end (blocking).
pub fn play_wav(data: &[u8]) -> Result<()> {
    let samples = decode_wav(data).ok_or(crate::Error(crate::abi::err::EINVAL))?;
    Stream::open()?.write(&samples)
}

/// Plays a system sound (a file in `/System/Sounds`, named without `.wav`)
/// in the background.
pub fn play_system(name: &str) {
    let _ = call(nr::DESKTOP, &[crate::abi::desktop::PLAY_SOUND as u64, name.as_ptr() as u64, name.len() as u64]);
}

/// Output state: volume (0–100), muted, device present, headphones.
#[derive(Clone, Copy, Debug)]
pub struct Volume {
    pub level: u32,
    pub muted: bool,
    pub present: bool,
    pub headphones: bool,
}

fn state(v: u64) -> Volume {
    Volume {
        level: (v & 0xFF) as u32,
        muted: v & abi::MUTED != 0,
        present: v & abi::PRESENT != 0,
        headphones: v & abi::HEADPHONES != 0,
    }
}

pub fn volume() -> Volume {
    state(call(nr::AUDIO_VOLUME, &[abi::GET]).unwrap_or(0))
}

/// Sets the system volume (saved as a preference).
pub fn set_volume(level: u32, muted: bool) -> Volume {
    state(call(nr::AUDIO_VOLUME, &[abi::SET, level.min(100) as u64, muted as u64]).unwrap_or(0))
}
