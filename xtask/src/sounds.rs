//! System sounds, synthesized at build time into `/System/Sounds/*.wav`
//! (48 kHz, 16-bit stereo, the mixer's native format).

use std::f32::consts::TAU;

const RATE: u32 = 48_000;

/// One struck note: a few decaying harmonics, like a small bell or marimba.
struct Note {
    freq: f32,
    at: f32,
    amp: f32,
    /// Exponential decay rate (1/s).
    decay: f32,
    /// Stereo position, -1 (left) to 1 (right).
    pan: f32,
}

fn note(freq: f32, at: f32, amp: f32, decay: f32, pan: f32) -> Note {
    Note { freq, at, amp, decay, pan }
}

/// Deterministic white noise.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1 << 23) as f32 - 1.0
    }
}

fn render(seconds: f32, notes: &[Note], mut extra: impl FnMut(f32) -> f32) -> Vec<i16> {
    let frames = (seconds * RATE as f32) as usize;
    let mut out = Vec::with_capacity(frames * 2);
    let mut left = vec![0f32; frames];
    let mut right = vec![0f32; frames];
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = i as f32 / RATE as f32;
        let mut sum_l = 0.0;
        let mut sum_r = 0.0;
        for n in notes {
            let dt = t - n.at;
            if dt < 0.0 {
                continue;
            }
            let attack = (dt / 0.004).min(1.0);
            let env = attack * (-dt * n.decay).exp() * n.amp;
            let wave = (TAU * n.freq * dt).sin()
                + 0.35 * (TAU * n.freq * 2.0 * dt).sin() * (-dt * n.decay * 1.5).exp()
                + 0.12 * (TAU * n.freq * 3.01 * dt).sin() * (-dt * n.decay * 2.5).exp();
            let v = wave * env;
            sum_l += v * (1.0 - n.pan) * 0.5;
            sum_r += v * (1.0 + n.pan) * 0.5;
        }
        let e = extra(t);
        *l = sum_l + e;
        *r = sum_r + e;
    }
    // Normalize, then fade the last 10 ms so nothing clicks.
    let peak = left.iter().chain(right.iter()).fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let gain = 0.55 * 32767.0 / peak;
    let fade = (RATE / 100) as usize;
    for i in 0..frames {
        let f = if i + fade > frames { (frames - i) as f32 / fade as f32 } else { 1.0 };
        out.push((left[i] * gain * f) as i16);
        out.push((right[i] * gain * f) as i16);
    }
    out
}

fn startup() -> Vec<i16> {
    // A rising major-seventh arpeggio that settles into a chord.
    let notes = [
        note(261.63, 0.00, 0.8, 1.6, -0.3),
        note(329.63, 0.09, 0.7, 1.7, -0.1),
        note(392.00, 0.18, 0.7, 1.8, 0.1),
        note(493.88, 0.27, 0.6, 1.9, 0.3),
        note(523.25, 0.36, 0.8, 1.5, 0.0),
        note(130.81, 0.36, 0.5, 1.2, 0.0),
    ];
    render(2.2, &notes, |_| 0.0)
}

fn notify() -> Vec<i16> {
    render(0.9, &[note(1318.5, 0.0, 0.8, 6.0, -0.2), note(1975.5, 0.11, 0.7, 5.0, 0.2)], |_| 0.0)
}

fn volume() -> Vec<i16> {
    render(0.12, &[note(1046.5, 0.0, 1.0, 38.0, 0.0)], |_| 0.0)
}

fn timer() -> Vec<i16> {
    let mut notes = Vec::new();
    for k in 0..4 {
        let t = k as f32 * 0.42;
        notes.push(note(1760.0, t, 0.8, 9.0, 0.0));
        notes.push(note(1760.0, t + 0.14, 0.8, 9.0, 0.0));
    }
    render(1.9, &notes, |_| 0.0)
}

fn error() -> Vec<i16> {
    render(0.45, &[note(220.0, 0.0, 1.0, 9.0, 0.0), note(207.65, 0.07, 0.8, 10.0, 0.0)], |_| 0.0)
}

fn trash() -> Vec<i16> {
    // Paper crumpling: short bursts of decaying noise.
    let mut noise = Noise(0x5eed);
    let bursts = [0.0f32, 0.05, 0.11, 0.14, 0.2, 0.27, 0.31];
    let mut prev = 0.0;
    render(0.45, &[], move |t| {
        let env: f32 = bursts.iter().filter(|&&b| t >= b).map(|&b| (-(t - b) * 60.0).exp()).sum();
        // One-pole high-pass keeps it crisp.
        let n = noise.next();
        let hp = n - prev * 0.6;
        prev = n;
        hp * env * 0.6
    })
}

fn screenshot() -> Vec<i16> {
    let mut noise = Noise(0xc1ac);
    render(0.25, &[], move |t| {
        let click = |at: f32| if t >= at { (-(t - at) * 90.0).exp() } else { 0.0 };
        noise.next() * (click(0.0) + 0.8 * click(0.09))
    })
}

/// (file name, WAV bytes) for every system sound.
pub fn all() -> Vec<(&'static str, Vec<u8>)> {
    let sounds: [(&str, fn() -> Vec<i16>); 7] = [
        ("startup.wav", startup),
        ("notify.wav", notify),
        ("volume.wav", volume),
        ("timer.wav", timer),
        ("error.wav", error),
        ("trash.wav", trash),
        ("screenshot.wav", screenshot),
    ];
    sounds.iter().map(|(name, f)| (*name, aurora_wav::encode(&f(), 2, RATE))).collect()
}
