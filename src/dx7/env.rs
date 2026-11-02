//! DX7 operator envelope (msfa `Env`, with Dexed's ACCURATE_ENVELOPE
//! static-segment timing).

use super::tables::{LG_N, N, Rates};

const LEVEL_LUT: [i32; 20] = [0, 5, 9, 13, 17, 20, 23, 25, 27, 29, 31, 33, 35, 37, 39, 41, 42, 43, 45, 46];

/// Samples at 44.1 kHz that a segment with no level change takes, by rate
/// (measured on a DX7 by the Dexed author, up to rate 76).
const STATICS: [i32; 77] = [
    1764000, 1764000, 1411200, 1411200, 1190700, 1014300, 992250, 882000, 705600, 705600, 584325,
    507150, 502740, 441000, 418950, 352800, 308700, 286650, 253575, 220500, 220500, 176400, 145530,
    145530, 125685, 110250, 110250, 88200, 88200, 74970, 61740, 61740, 55125, 48510, 44100, 37485,
    31311, 30870, 27562, 27562, 22050, 18522, 17640, 15435, 14112, 13230, 11025, 9261, 9261, 7717,
    6615, 6615, 5512, 5512, 4410, 3969, 3969, 3439, 2866, 2690, 2249, 1984, 1896, 1808, 1411, 1367,
    1234, 1146, 926, 837, 837, 705, 573, 573, 529, 441, 441,
];

pub fn scale_out_level(outlevel: i32) -> i32 {
    if outlevel >= 20 { 28 + outlevel } else { LEVEL_LUT[outlevel.max(0) as usize] }
}

#[derive(Clone, Copy, Default)]
pub struct Env {
    initialised: bool,
    rates: [i32; 4],
    levels: [i32; 4],
    outlevel: i32,
    rate_scaling: i32,
    /// Q24 log level (2^24 = one doubling).
    level: i32,
    target: i32,
    rising: bool,
    ix: usize,
    inc: i32,
    static_count: i32,
    down: bool,
    sr_mult: i64,
}

impl Env {
    pub fn init(&mut self, r: &Rates, rates: [i32; 4], levels: [i32; 4], outlevel: i32, rate_scaling: i32) {
        self.initialised = true;
        self.sr_mult = r.env_sr_mult;
        self.rates = rates;
        self.levels = levels;
        self.outlevel = outlevel;
        self.rate_scaling = rate_scaling;
        self.level = 0;
        self.down = true;
        self.advance(0);
    }

    /// Live parameter edit on a sounding note.
    pub fn update(&mut self, rates: [i32; 4], levels: [i32; 4], outlevel: i32, rate_scaling: i32) {
        self.rates = rates;
        self.levels = levels;
        self.outlevel = outlevel;
        self.rate_scaling = rate_scaling;
        if self.down {
            // Dexed: simply re-target the sustain segment.
            let actual = ((scale_out_level(self.levels[2]) >> 1) << 6) - 4256;
            self.target = actual.max(16) << 16;
            self.advance(2);
        }
    }

    /// One value per block of N samples.
    #[inline]
    pub fn getsample(&mut self) -> i32 {
        if self.static_count > 0 {
            self.static_count -= N as i32;
            if self.static_count <= 0 {
                self.static_count = 0;
                self.advance(self.ix + 1);
            }
        }
        if self.ix < 3 || (self.ix < 4 && !self.down) {
            if self.static_count > 0 {
            } else if self.rising {
                const JUMP_TARGET: i32 = 1716;
                if self.level < (JUMP_TARGET << 16) {
                    self.level = JUMP_TARGET << 16;
                }
                self.level = self.level.wrapping_add((((17 << 24) - self.level) >> 24) * self.inc);
                if self.level >= self.target {
                    self.level = self.target;
                    self.advance(self.ix + 1);
                }
            } else {
                self.level -= self.inc;
                if self.level <= self.target {
                    self.level = self.target;
                    self.advance(self.ix + 1);
                }
            }
        }
        self.level
    }

    pub fn keydown(&mut self, d: bool) {
        if self.down != d {
            self.down = d;
            self.advance(if d { 0 } else { 3 });
        }
    }

    fn advance(&mut self, newix: usize) {
        self.ix = newix;
        if self.ix >= 4 {
            return;
        }
        let newlevel = self.levels[self.ix];
        let actual = ((scale_out_level(newlevel) >> 1) << 6) + self.outlevel - 4256;
        let actual = actual.max(16);
        self.target = actual << 16;
        self.rising = self.target > self.level;

        let qrate = (((self.rates[self.ix] * 41) >> 6) + self.rate_scaling).min(63);

        if self.target == self.level || (self.ix == 0 && newlevel == 0) {
            let staticrate = (self.rates[self.ix] + self.rate_scaling).min(99);
            let mut sc = if staticrate < 77 { STATICS[staticrate.max(0) as usize] } else { 20 * (99 - staticrate) };
            if staticrate < 77 && self.ix == 0 && newlevel == 0 {
                sc /= 20; // attack is scaled faster
            }
            self.static_count = ((i64::from(sc) * self.sr_mult) >> 24) as i32;
        } else {
            self.static_count = 0;
        }
        let inc = (4 + (qrate & 3)) << (2 + LG_N + (qrate >> 2));
        self.inc = ((i64::from(inc) * self.sr_mult) >> 24) as i32;
    }

    pub fn transfer(&mut self, src: &Env) {
        *self = *src;
    }

    /// Active until L4 is reached, or forever when L4 > 0.
    pub fn is_active(&self) -> bool {
        self.initialised && (self.ix < 4 || self.levels[3] > 0)
    }

    pub fn stage(&self) -> usize {
        self.ix
    }
}

/// DX7 pitch envelope (msfa `PitchEnv`). Output in Q24 octaves.
#[derive(Clone, Copy, Default)]
pub struct PitchEnv {
    rates: [i32; 4],
    levels: [i32; 4],
    level: i32,
    target: i32,
    rising: bool,
    ix: usize,
    inc: i32,
    down: bool,
    unit: i32,
}

pub const PITCHENV_RATE: [u8; 100] = [
    1, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 16, 16,
    17, 18, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 30, 31, 33, 34, 36, 37, 38, 39, 41, 42, 44, 46,
    47, 49, 51, 53, 54, 56, 58, 60, 62, 64, 66, 68, 70, 72, 74, 76, 79, 82, 85, 88, 91, 94, 98, 102, 106,
    110, 115, 120, 125, 130, 135, 141, 147, 153, 159, 165, 171, 178, 185, 193, 202, 211, 232, 243, 254,
    255,
];

pub const PITCHENV_TAB: [i8; 100] = [
    -128, -116, -104, -95, -85, -76, -68, -61, -56, -52, -49, -46, -43, -41, -39, -37, -35, -33, -32,
    -31, -30, -29, -28, -27, -26, -25, -24, -23, -22, -21, -20, -19, -18, -17, -16, -15, -14, -13, -12,
    -11, -10, -9, -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 38, 40, 43, 46, 49, 53,
    58, 65, 73, 82, 92, 103, 115, 127,
];

impl PitchEnv {
    pub fn set(&mut self, r: &Rates, rates: [i32; 4], levels: [i32; 4]) {
        self.unit = r.pitchenv_unit;
        self.rates = rates;
        self.levels = levels;
        self.level = i32::from(PITCHENV_TAB[levels[3].clamp(0, 99) as usize]) << 19;
        self.down = true;
        self.advance(0);
    }

    #[inline]
    pub fn getsample(&mut self) -> i32 {
        if self.ix < 3 || (self.ix < 4 && !self.down) {
            if self.rising {
                self.level += self.inc;
                if self.level >= self.target {
                    self.level = self.target;
                    self.advance(self.ix + 1);
                }
            } else {
                self.level -= self.inc;
                if self.level <= self.target {
                    self.level = self.target;
                    self.advance(self.ix + 1);
                }
            }
        }
        self.level
    }

    pub fn keydown(&mut self, d: bool) {
        if self.down != d {
            self.down = d;
            self.advance(if d { 0 } else { 3 });
        }
    }

    fn advance(&mut self, newix: usize) {
        self.ix = newix;
        if self.ix < 4 {
            let newlevel = self.levels[self.ix].clamp(0, 99) as usize;
            self.target = i32::from(PITCHENV_TAB[newlevel]) << 19;
            self.rising = self.target > self.level;
            self.inc = i32::from(PITCHENV_RATE[self.rates[self.ix].clamp(0, 99) as usize]) * self.unit;
        }
    }
}
