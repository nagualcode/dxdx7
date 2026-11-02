//! Lookup tables of the msfa / Dexed engine, built once.
//!
//! Fixed-point conventions (inherited from msfa):
//! - phase: Q24, `1 << 24` = one cycle
//! - logfreq: Q24 log2(Hz), `1 << 24` = one octave
//! - envelope level: Q24 log2 gain, `1 << 24` = 6.02 dB

use std::sync::OnceLock;

pub const LG_N: i32 = 6;
/// Samples per engine block (envelopes, LFO and controllers update once per block).
pub const N: usize = 1 << LG_N;

const SIN_LG_N: i32 = 10;
const SIN_N: usize = 1 << SIN_LG_N;
const EXP2_LG_N: i32 = 10;
const EXP2_N: usize = 1 << EXP2_LG_N;

pub struct Tables {
    /// Interleaved (delta, value) sine table, msfa `SIN_DELTA` layout.
    sin: [i32; SIN_N * 2],
    /// Interleaved (delta, value) 2^x table.
    exp2: [i32; EXP2_N * 2],
    /// Mark I: quarter-wave -log2(sin) table, 10-bit attenuation units (1024 = 6 dB).
    pub sin_log: [u16; 1024],
    /// Mark I: fractional 2^x table (0..4096).
    pub sin_exp: [u16; 1024],
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Box<Tables>> = OnceLock::new();
    T.get_or_init(build)
}

fn build() -> Box<Tables> {
    let mut t = Box::new(Tables { sin: [0; SIN_N * 2], exp2: [0; EXP2_N * 2], sin_log: [0; 1024], sin_exp: [0; 1024] });

    // Sin::init (rotation recurrence, exactly as msfa so outputs match bit for bit).
    let dphase = 2.0 * std::f64::consts::PI / SIN_N as f64;
    let c = (dphase.cos() * f64::from(1 << 30) + 0.5).floor() as i32;
    let s = (dphase.sin() * f64::from(1 << 30) + 0.5).floor() as i32;
    const R: i64 = 1 << 29;
    let mut u: i32 = 1 << 30;
    let mut v: i32 = 0;
    for i in 0..SIN_N / 2 {
        t.sin[(i << 1) + 1] = (v + 32) >> 6;
        t.sin[((i + SIN_N / 2) << 1) + 1] = -((v + 32) >> 6);
        let tt = ((i64::from(u) * i64::from(s) + i64::from(v) * i64::from(c) + R) >> 30) as i32;
        u = ((i64::from(u) * i64::from(c) - i64::from(v) * i64::from(s) + R) >> 30) as i32;
        v = tt;
    }
    for i in 0..SIN_N - 1 {
        t.sin[i << 1] = t.sin[(i << 1) + 3] - t.sin[(i << 1) + 1];
    }
    t.sin[(SIN_N << 1) - 2] = -t.sin[(SIN_N << 1) - 1];

    // Exp2::init
    let inc = 2f64.powf(1.0 / EXP2_N as f64);
    let mut y = f64::from(1 << 30);
    for i in 0..EXP2_N {
        t.exp2[(i << 1) + 1] = (y + 0.5).floor() as i32;
        y *= inc;
    }
    for i in 0..EXP2_N - 1 {
        t.exp2[i << 1] = t.exp2[(i << 1) + 3] - t.exp2[(i << 1) + 1];
    }
    t.exp2[(EXP2_N << 1) - 2] = ((1u32 << 31) as i64 - i64::from(t.exp2[(EXP2_N << 1) - 1])) as i32;

    // EngineMkI tables (computed in f32 like the original).
    for i in 0..1024 {
        let x1 = (((0.5 + i as f32) / 1024.0) * std::f32::consts::FRAC_PI_2).sin();
        t.sin_log[i] = (-1024.0 * x1.log2()).round() as u16;
        let x2 = (2f32.powf(i as f32 / 1024.0) - 1.0) * 4096.0;
        t.sin_exp[i] = x2.round() as u16;
    }
    t
}

impl Tables {
    /// Q24 phase in, Q24 sine out.
    #[inline(always)]
    pub fn sin(&self, phase: i32) -> i32 {
        const SHIFT: i32 = 24 - SIN_LG_N;
        let lowbits = phase & ((1 << SHIFT) - 1);
        let ix = ((phase >> (SHIFT - 1)) & (((SIN_N - 1) << 1) as i32)) as usize;
        let dy = self.sin[ix];
        let y0 = self.sin[ix + 1];
        y0 + ((i64::from(dy) * i64::from(lowbits)) >> SHIFT) as i32
    }

    /// Q24 in, Q24 out: 2^x.
    #[inline(always)]
    pub fn exp2(&self, x: i32) -> i32 {
        const SHIFT: i32 = 24 - EXP2_LG_N;
        let lowbits = x & ((1 << SHIFT) - 1);
        let ix = ((x >> (SHIFT - 1)) & (((EXP2_N - 1) << 1) as i32)) as usize;
        let dy = self.exp2[ix];
        let y0 = self.exp2[ix + 1];
        let y = y0 + ((i64::from(dy) * i64::from(lowbits)) >> SHIFT) as i32;
        let sh = 6 - (x >> 24);
        if sh >= 31 {
            0
        } else if sh <= 0 {
            y
        } else {
            y >> sh
        }
    }

    /// Mark I log-sin lookup: 12-bit phase (4 quadrants x 1024) to
    /// attenuation with the sign in bit 15.
    #[inline(always)]
    pub fn mki_sin_log(&self, phi: u16) -> u16 {
        const NEG: u16 = 0x8000;
        let index = (phi & 1023) as usize;
        match phi & (1024 * 3) {
            0 => self.sin_log[index],
            1024 => self.sin_log[index ^ 1023],
            2048 => self.sin_log[index] | NEG,
            _ => self.sin_log[index ^ 1023] | NEG,
        }
    }

    /// Mark I operator: phase (Q24) and envelope attenuation (14-bit, 1024
    /// per 6 dB) to a signed sample (about +-2^26 at full level).
    #[inline(always)]
    pub fn mki_sin(&self, phase: i32, env: u16) -> i32 {
        let mut exp_val = self.mki_sin_log((phase >> (22 - 10)) as u16).wrapping_add(env);
        let signed = exp_val & 0x8000 != 0;
        exp_val &= 0x7FFF;
        let mut result: u32 = 4096 + u32::from(self.sin_exp[((exp_val & 0x3FF) ^ 0x3FF) as usize]);
        let sh = u32::from(exp_val >> 10);
        result = if sh >= 16 { 0 } else { result >> sh };
        if signed { (-(result as i32) - 1) << 13 } else { (result as i32) << 13 }
    }
}

/// Resolve a Q24 log2 frequency to a Q24 phase increment at the engine rate.
pub struct FreqLut {
    lut: [i64; 1025],
}

impl FreqLut {
    pub fn new(sample_rate: f64) -> Self {
        let mut lut = [0i64; 1025];
        let mut y = (1u64 << 44) as f64 / sample_rate;
        let inc = 2f64.powf(1.0 / 1024.0);
        for v in lut.iter_mut() {
            *v = (y + 0.5).floor() as i64;
            y *= inc;
        }
        Self { lut }
    }

    #[inline(always)]
    pub fn lookup(&self, logfreq: i32) -> i32 {
        const SAMPLE_SHIFT: i32 = 24 - 10;
        let ix = ((logfreq & 0xff_ffff) >> SAMPLE_SHIFT) as usize;
        let y0 = self.lut[ix];
        let y1 = self.lut[ix + 1];
        let lowbits = i64::from(logfreq & ((1 << SAMPLE_SHIFT) - 1));
        let y = y0 + (((y1 - y0) * lowbits) >> SAMPLE_SHIFT);
        let hibits = logfreq >> 24;
        let sh = 20 - hibits;
        let r = if sh <= 0 { y << (-sh).min(8) } else if sh >= 63 { 0 } else { y >> sh };
        r as i32
    }
}

/// Sample-rate-dependent constants shared by every voice.
pub struct Rates {
    pub sample_rate: f64,
    pub freq: FreqLut,
    /// Envelope rate multiplier, Q24 (the envelope code is normalised to 44.1 kHz).
    pub env_sr_mult: i64,
    /// Pitch EG unit increment.
    pub pitchenv_unit: i32,
    pub lfo_unit: u32,
    pub lfo_ratio: u32,
    pub porta: [i32; 128],
    pub porta_gliss: [i32; 128],
}

impl Rates {
    pub fn new(sample_rate: f64) -> Self {
        let n = N as f64;
        let mut porta = [0; 128];
        let mut porta_gliss = [0; 128];
        let step = f64::from((1 << 24) / 12);
        for i in 0..128 {
            let sps = 2100.0 * 2f64.powf(-0.062 * i as f64);
            porta[i] = (0.5 + step * sps / sample_rate * n) as i32;
            let sps = 1300.0 * 2f64.powf(-0.062 * i as f64);
            porta_gliss[i] = (0.5 + step * sps / sample_rate * n) as i32;
        }
        Self {
            sample_rate,
            freq: FreqLut::new(sample_rate),
            env_sr_mult: ((44100.0 / sample_rate) * f64::from(1 << 24)) as i64,
            pitchenv_unit: (n * f64::from(1 << 24) / (21.3 * sample_rate) + 0.5) as i32,
            lfo_unit: (n * 25_190_424.0 / sample_rate + 0.5) as u32,
            lfo_ratio: (4_437_500_000.0 * n / sample_rate) as u32,
            porta,
            porta_gliss,
        }
    }
}
