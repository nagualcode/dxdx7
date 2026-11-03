//! Converts the DX7's 49.1 kHz output to the host rate. The DAC
//! sample-and-hold and the analog reconstruction filter are built into
//! the interpolation kernel.
//!
//! The DX7's output filter has poles at 11.6 and 16 kHz, close to the host
//! Nyquist. A bilinear-transform IIR at 44.1/48 kHz gets that region badly
//! wrong (about -7 dB at 15 kHz instead of +0.8 dB). Instead, the kernel is
//! designed by frequency sampling from the analog magnitude response:
//!
//!   D(f) = |H_analog(f)| * |sinc(f / 49096)| * anti-image taper
//!
//! (zero-phase, windowed), evaluated on a fine grid of fractional delays.
//! The result matches the analog response at every host rate up to the
//! host's own Nyquist.

const HALF_TAPS: usize = 32;
const TAPS: usize = 2 * HALF_TAPS;
const PHASES: usize = 256;
const HIST: usize = 128;

pub struct Resampler {
    /// Two kernels (plain S&H, S&H + analog filter), each `PHASES + 1`
    /// rows of `TAPS` coefficients (extra row for interpolation).
    kernels: [Vec<[f32; TAPS]>; 2],
    /// Use the kernel with the analog filter embedded.
    pub filter: bool,
    hist: [f32; HIST],
    w: usize,
    /// Core samples consumed per host sample.
    ratio: f64,
    frac: f64,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..60 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

/// Target magnitude at `f` Hz.
pub type Response = fn(f64) -> f64;

impl Resampler {
    /// Builds both kernels up front (tens of ms): call off the audio thread.
    /// `analog`: magnitude response embedded in the filtered kernel.
    pub fn new(core_sr: f64, host_sr: f64, analog: Response) -> Self {
        let ratio = core_sr / host_sr;
        let kernels = [build(core_sr, host_sr, None), build(core_sr, host_sr, Some(analog))];
        Self { kernels, filter: true, hist: [0.0; HIST], w: 0, ratio, frac: 0.0 }
    }
}

fn build(core_sr: f64, host_sr: f64, analog: Option<Response>) -> Vec<[f32; TAPS]> {
    let zoh = true;
    {
        // Anti-image taper: flat to `pass`, raised cosine to zero at `stop`
        // (both capped by the core and host Nyquist).
        let nyq = 0.5 * core_sr.min(host_sr);
        let stop = nyq.min(24_000.0);
        let pass = (stop * 0.9).min(20_500.0);
        let resp = |f: f64| -> f64 {
            if f >= stop {
                return 0.0;
            }
            let mut d = 1.0;
            if f > pass {
                d *= 0.5 * (1.0 + (std::f64::consts::PI * (f - pass) / (stop - pass)).cos());
            }
            if zoh && f > 0.0 {
                let x = std::f64::consts::PI * f / core_sr;
                d *= x.sin() / x;
            }
            if let Some(h) = analog {
                d *= h(f);
            }
            d
        };
        // Frequency grid (Hz) for the inverse transform.
        const NF: usize = 768;
        let df = stop / NF as f64;
        let spectrum: Vec<(f64, f64)> = (0..NF)
            .map(|i| {
                let f = (i as f64 + 0.5) * df;
                (f / core_sr, resp(f) * df / core_sr)
            })
            .collect();
        let beta = 7.0;
        let i0b = bessel_i0(beta);
        // Continuous zero-phase impulse at offset x core samples.
        let h = |x: f64| -> f64 {
            let t = x / HALF_TAPS as f64;
            if t.abs() >= 1.0 {
                return 0.0;
            }
            let w = bessel_i0(beta * (1.0 - t * t).sqrt()) / i0b;
            let mut acc = 0.0;
            for &(fn_, a) in &spectrum {
                acc += a * (2.0 * std::f64::consts::PI * fn_ * x).cos();
            }
            2.0 * acc * w
        };
        let mut kernel = vec![[0f32; TAPS]; PHASES + 1];
        for (ph, row) in kernel.iter_mut().enumerate() {
            let mu = ph as f64 / PHASES as f64;
            let mut tmp = [0f64; TAPS];
            let mut sum = 0.0;
            for (k, c) in tmp.iter_mut().enumerate() {
                // Tap k reads sample (newest - k).
                *c = h(k as f64 - (HALF_TAPS as f64 - 1.0) - mu);
                sum += *c;
            }
            for (c, t) in row.iter_mut().zip(tmp) {
                *c = (t / sum) as f32; // unity DC gain at every phase
            }
        }
        kernel
    }
}

impl Resampler {

    /// Produce one host-rate sample, pulling core-rate samples from `pull`.
    #[inline]
    /// Forget buffered input (after all voices were cut).
    pub fn clear(&mut self) {
        self.hist = [0.0; HIST];
    }

    pub fn next(&mut self, mut pull: impl FnMut() -> f32) -> f32 {
        self.frac += self.ratio;
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            self.w = (self.w + 1) & (HIST - 1);
            self.hist[self.w] = pull();
        }
        // Read point lies `1 - frac` samples before the newest one.
        let pos = (1.0 - self.frac) * PHASES as f64;
        let ip = (pos as usize).min(PHASES - 1);
        let t = (pos - ip as f64) as f32;
        let kernel = &self.kernels[usize::from(self.filter)];
        let a = &kernel[ip];
        let b = &kernel[ip + 1];
        let mut acc = 0.0f32;
        for k in 0..TAPS {
            let c = a[k] + (b[k] - a[k]) * t;
            acc += c * self.hist[(self.w + HIST - k) & (HIST - 1)];
        }
        acc
    }
}
