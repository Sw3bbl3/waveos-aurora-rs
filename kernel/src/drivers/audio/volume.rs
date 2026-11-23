//! The volume curve in decibels, for devices with their own volume control.
//!
//! The mixer's software volume is a square curve: gain = (v / 100)². In
//! decibels that is 40·log10(v / 100), which is what a device's volume
//! control (in 1/256 dB, as USB Audio counts) is set to. When the device
//! can't go that low, the mixer makes up the rest. Integer tables: the
//! kernel does no floating point.

/// 40·log10(v / 100) in 1/256 dB for v = 0…100 (0 is silence).
const VOLUME_DB: [i16; 101] = [
    -32768, -20480, -17397, -15594, -14315, -13323, -12512, -11826, -11232, -10709, -10240, -9816, -9429, -9073, -8744,
    -8437, -8150, -7880, -7626, -7386, -7157, -6940, -6734, -6536, -6347, -6165, -5991, -5823, -5661, -5505, -5354,
    -5208, -5067, -4930, -4798, -4669, -4543, -4422, -4303, -4187, -4075, -3965, -3858, -3753, -3651, -3551, -3453,
    -3358, -3264, -3172, -3083, -2994, -2908, -2823, -2740, -2659, -2579, -2500, -2422, -2346, -2272, -2198, -2126,
    -2055, -1985, -1916, -1848, -1781, -1715, -1650, -1586, -1523, -1461, -1400, -1339, -1279, -1220, -1162, -1105,
    -1048, -992, -937, -883, -829, -775, -723, -671, -619, -568, -518, -469, -419, -371, -323, -275, -228, -182, -135,
    -90, -45, 0,
];

/// 10^(-k/20) in 16.16 for k = 0…96 dB.
const ATTENUATION: [u32; 97] = [
    65536, 58409, 52057, 46396, 41350, 36854, 32846, 29274, 26090, 23253, 20724, 18471, 16462, 14672, 13076, 11654,
    10387, 9257, 8250, 7353, 6554, 5841, 5206, 4640, 4135, 3685, 3285, 2927, 2609, 2325, 2072, 1847, 1646, 1467, 1308,
    1165, 1039, 926, 825, 735, 655, 584, 521, 464, 414, 369, 328, 293, 261, 233, 207, 185, 165, 147, 131, 117, 104, 93,
    83, 74, 66, 58, 52, 46, 41, 37, 33, 29, 26, 23, 21, 18, 16, 15, 13, 12, 10, 9, 8, 7, 7, 6, 5, 5, 4, 4, 3, 3, 3, 2,
    2, 2, 2, 1, 1, 1, 1,
];

/// The level for volume `v` (0–100) in 1/256 dB.
pub fn db(v: u32) -> i32 {
    VOLUME_DB[v.min(100) as usize] as i32
}

/// The software gain (16.16) for volume `v` without a device control.
pub fn software_gain(v: u32) -> u32 {
    let v = v.min(100);
    v * v * 65536 / 10_000
}

/// A gain of `db256` (1/256 dB, at most 0) as a 16.16 factor.
pub fn gain_q16(db256: i32) -> u32 {
    let a = (-db256).max(0) as u32;
    let (whole, frac) = ((a / 256) as usize, a % 256);
    if whole >= ATTENUATION.len() - 1 {
        return 0;
    }
    let (hi, lo) = (ATTENUATION[whole], ATTENUATION[whole + 1]);
    hi - (hi - lo) * frac / 256
}

/// Splits volume `v` between a device control spanning `min..=max` (1/256
/// dB) and the mixer: returns the device setting and the mixer's 16.16 gain.
pub fn split(v: u32, min: i16, max: i16) -> (i16, u32) {
    let target = db(v);
    let (min, max) = (min as i32, (max as i32).min(0).max(min as i32));
    let hw = target.clamp(min, max);
    (hw as i16, gain_q16((target - hw).min(0)))
}
