//! DX7 LFO (msfa `Lfo`): one global LFO, updated once per block.

use super::tables::{Rates, tables};

/// LFO frequency in Hz for speed 0..99 (measured on a DX7).
pub const LFO_HZ: [f64; 100] = [
    0.062541, 0.125031, 0.312393, 0.437120, 0.624610, 0.750694, 0.936330, 1.125302, 1.249609, 1.436782,
    1.560915, 1.752081, 1.875117, 2.062494, 2.247191, 2.374451, 2.560492, 2.686728, 2.873976, 2.998950,
    3.188013, 3.369840, 3.500175, 3.682224, 3.812065, 4.000800, 4.186202, 4.310716, 4.501260, 4.623209,
    4.814636, 4.930480, 5.121901, 5.315191, 5.434783, 5.617346, 5.750431, 5.946717, 6.062811, 6.248438,
    6.431695, 6.564264, 6.749460, 6.868132, 7.052186, 7.250580, 7.375719, 7.556294, 7.687577, 7.877738,
    7.993605, 8.181967, 8.372405, 8.504848, 8.685079, 8.810573, 8.986341, 9.122423, 9.300595, 9.500285,
    9.607994, 9.798158, 9.950249, 10.117361, 11.251125, 11.384335, 12.562814, 13.676149, 13.904338,
    15.092062, 16.366612, 16.638935, 17.869907, 19.193858, 19.425019, 20.833333, 21.034918, 22.502250,
    24.003841, 24.260068, 25.746653, 27.173913, 27.578599, 29.052876, 30.693677, 31.191516, 32.658393,
    34.317090, 34.674064, 36.416606, 38.197097, 38.550501, 40.387722, 40.749796, 42.625746, 44.326241,
    44.883303, 46.772685, 48.590865, 49.261084,
];

#[derive(Default)]
pub struct Lfo {
    phase: u32,
    delta: u32,
    waveform: u8,
    randstate: u8,
    sync: bool,
    delaystate: u32,
    delayinc: u32,
    delayinc2: u32,
}

impl Lfo {
    /// `p` = voice bytes 137..=142 (speed, delay, PMD, AMD, sync, wave).
    pub fn reset(&mut self, r: &Rates, p: &[u8]) {
        let rate = usize::from(p[0].min(99));
        self.delta = (LFO_HZ[rate] * f64::from(r.lfo_ratio)) as u32;
        let a = 99 - i32::from(p[1].min(99));
        if a == 99 {
            self.delayinc = u32::MAX;
            self.delayinc2 = u32::MAX;
        } else {
            let a = (16 + (a & 15)) << (1 + (a >> 4));
            self.delayinc = r.lfo_unit.wrapping_mul(a as u32);
            let a2 = (a & 0xff80).max(0x80);
            self.delayinc2 = r.lfo_unit.wrapping_mul(a2 as u32);
        }
        self.waveform = p[5];
        self.sync = p[4] != 0;
    }

    /// 0..1 in Q24.
    pub fn getsample(&mut self) -> i32 {
        self.phase = self.phase.wrapping_add(self.delta);
        let phase = self.phase;
        match self.waveform {
            0 => {
                let mut x = (phase >> 7) as i32;
                x ^= (phase >> 31).wrapping_neg() as i32;
                x & ((1 << 24) - 1)
            }
            1 => ((!phase ^ (1u32 << 31)) >> 8) as i32,
            2 => ((phase ^ (1u32 << 31)) >> 8) as i32,
            3 => (((!phase) >> 7) & (1 << 24)) as i32,
            4 => (1 << 23) + (tables().sin((phase >> 8) as i32) >> 1),
            5 => {
                if phase < self.delta {
                    self.randstate = (u32::from(self.randstate) * 179 + 17) as u8;
                }
                let x = i32::from(self.randstate ^ 0x80);
                (x + 1) << 16
            }
            _ => 1 << 23,
        }
    }

    /// Delay ramp, 0..1 in Q24.
    pub fn getdelay(&mut self) -> i32 {
        let delta = if self.delaystate < (1u32 << 31) { self.delayinc } else { self.delayinc2 };
        let d = u64::from(self.delaystate) + u64::from(delta);
        if d > u64::from(u32::MAX) {
            return 1 << 24;
        }
        self.delaystate = d as u32;
        if d < (1u64 << 31) { 0 } else { ((d >> 7) & ((1 << 24) - 1)) as i32 }
    }

    pub fn keydown(&mut self) {
        if self.sync {
            self.phase = (1u32 << 31) - 1;
        }
        self.delaystate = 0;
    }
}
