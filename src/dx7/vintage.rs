//! DX7 Mark I output path: what happens to the sound after the OPS chip.
//! Constants are documented in `docs/HARDWARE.md` with their sources.
//!
//! core rate (49.1 kHz)                     host rate
//! each voice -> own DAC conversion --> S&H + resample -> output filter -> out amp
//!               (sign + 11-bit mantissa   (ZOH droop)     (5th-order       (hiss,
//!                + 2-bit exponent,                         reconstruction   coupling cap,
//!                `synth::dx7_dac`)                         low-pass)        optional drive)

/// DX7 sample rate: 9.4265 MHz master clock / 192.
pub const DX7_SAMPLE_RATE: f64 = 9_426_500.0 / 192.0;

/// One-pole TPT low-pass.
#[derive(Clone, Copy, Default)]
struct OnePole {
    g: f32,
    s: f32,
}

impl OnePole {
    fn new(fc: f64, sr: f64) -> Self {
        let g = (std::f64::consts::PI * (fc / sr).min(0.49)).tan();
        Self { g: (g / (1.0 + g)) as f32, s: 0.0 }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let v = (x - self.s) * self.g;
        let y = v + self.s;
        self.s = y + v;
        y
    }
}

/// DX7 reconstruction filter: passive pole + two Sallen-Key stages
/// (5th order). Analog poles derived from VDX7's circuit model:
/// real pole 6788 Hz, pairs 11588 Hz / Q 1.017 and 16062 Hz / Q 4.13.
/// Response: flat to ~5 kHz, +0.8 dB at 15 kHz, -3 dB at 17 kHz,
/// -14 dB at 20 kHz.
pub const OUT_POLE_HZ: f64 = 6788.0;
pub const OUT_PAIRS: [(f64, f64); 2] = [(11588.0, 1.017), (16062.0, 4.13)];

/// Magnitude of the analog output filter at `f` Hz (embedded in the
/// resampler kernel, see `resample.rs`).
pub fn output_filter_response(f: f64) -> f64 {
    let w = f / OUT_POLE_HZ;
    let mut h = 1.0 / (1.0 + w * w).sqrt();
    for (fc, q) in OUT_PAIRS {
        let w = f / fc;
        let re = 1.0 - w * w;
        let im = w / q;
        h /= (re * re + im * im).sqrt();
    }
    h
}

/// Analog output stage at the host rate (after the reconstruction filter).
pub struct Analog {
    hiss_lp: OnePole,
    hp_x: f32,
    hp_y: f32,
    hp_r: f32,
    rng: u32,
    pink: [f32; 3],
    pub noise: f32,
    pub drive: f32,
}

/// Output coupling capacitor corner (not documented; typical value).
const HPF_HZ: f64 = 8.0;
/// Hiss at the output relative to one full-level carrier (0.125 FS):
/// about -84 dBFS RMS. No published measurement exists - this is an
/// estimate; set Analog Noise to 0 for a clean output.
const HISS: f32 = 4.0e-5;

impl Analog {
    pub fn new(sr: f64) -> Self {
        Self {
            // The hiss comes from the op-amps around the filter, so it is
            // band-limited too.
            hiss_lp: OnePole::new(12_000.0, sr),
            hp_x: 0.0,
            hp_y: 0.0,
            hp_r: (1.0 - 2.0 * std::f64::consts::PI * HPF_HZ / sr) as f32,
            rng: 0x1234_5678,
            pink: [0.0; 3],
            noise: 1.0,
            drive: 0.0,
        }
    }

    #[inline]
    fn white(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let mut y = x;
        if self.noise > 0.0 {
            // Op-amp hiss: white plus a little 1/f.
            let w = self.white();
            self.pink[0] = 0.997 * self.pink[0] + 0.029 * w;
            self.pink[1] = 0.985 * self.pink[1] + 0.032 * w;
            self.pink[2] = 0.950 * self.pink[2] + 0.048 * w;
            let n = 0.6 * w + self.pink[0] + self.pink[1] + self.pink[2];
            y += self.noise * HISS * self.hiss_lp.process(n);
        }
        if self.drive > 0.0 {
            // Output op-amp running out of headroom: unity small-signal gain.
            let d = 1.0 + 8.0 * self.drive;
            y = (y * d).tanh() / d;
        }
        let out = y - self.hp_x + self.hp_r * self.hp_y;
        self.hp_x = y;
        self.hp_y = out;
        out
    }
}
