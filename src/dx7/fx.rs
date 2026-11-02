//! Dexed's global output filter (`PluginFx`): a DC blocker, then an
//! OB-Xd-style 4-pole resonant low-pass on the Cutoff / Resonance knobs.
//! The DX7 itself has no filter; at Cutoff = max the filter is bypassed.

pub struct PluginFx {
    s1: f32,
    s2: f32,
    s3: f32,
    s4: f32,
    sr_inv: f32,
    d: f32,
    c: f32,
    r24: f32,
    rcor24: f32,
    rcor24_inv: f32,
    bright: f32,
    r_cutoff: f32,
    r_reso: f32,
    s_cutoff: f32,
    s_reso: f32,
    smooth: f32,
    bypassed: bool,
    p_cutoff: f32,
    p_reso: f32,
    dc_id: f32,
    dc_od: f32,
    dc_r: f32,
    fade: f32,
    fade_step: f32,
    fade_left: u32,
    pub ui_cutoff: f32,
    pub ui_reso: f32,
}

#[inline]
fn tptpc(state: &mut f32, inp: f32, cutoff: f32) -> f32 {
    let v = (inp - *state) * cutoff / (1.0 + cutoff);
    let res = v + *state;
    *state = res + v;
    res
}

#[inline]
fn tptlpupw(state: &mut f32, inp: f32, cutoff: f32, sr_inv: f32) -> f32 {
    let cutoff = cutoff * sr_inv * std::f32::consts::PI;
    tptpc(state, inp, cutoff)
}

fn logsc(param: f32, min: f32, max: f32) -> f32 {
    const ROLLOFF: f32 = 19.0;
    ((param * (ROLLOFF + 1.0).ln()).exp() - 1.0) / ROLLOFF * (max - min) + min
}

/// Display helper: cutoff knob (0..1) to Hz.
pub fn cutoff_hz(v: f32) -> f32 {
    logsc(v, 60.0, 19000.0)
}

impl PluginFx {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate;
        let rcrate = (44000.0 / sr).sqrt();
        let rcor24 = (970.0 / 44000.0) * rcrate;
        Self {
            s1: 0.0,
            s2: 0.0,
            s3: 0.0,
            s4: 0.0,
            sr_inv: 1.0 / sr,
            d: 0.0,
            c: 0.0,
            r24: 0.0,
            rcor24,
            rcor24_inv: 1.0 / rcor24,
            bright: ((sr * 0.5 - 10.0) * std::f32::consts::PI / sr).tan(),
            r_cutoff: 0.0,
            r_reso: 0.0,
            s_cutoff: (19000.0 / sr * std::f32::consts::PI).tan(),
            s_reso: 0.0,
            smooth: (-1.0 / (0.01 * sr)).exp(),
            bypassed: true,
            p_cutoff: -1.0,
            p_reso: -1.0,
            dc_id: 0.0,
            dc_od: 0.0,
            dc_r: 1.0 - 126.0 / sr,
            fade: 1.0,
            fade_step: 0.0,
            fade_left: 0,
            ui_cutoff: 1.0,
            ui_reso: 0.0,
        }
    }

    #[inline]
    fn nr24(&self, sample: f32, g: f32, lpc: f32) -> f32 {
        let ml = 1.0 / (1.0 + g);
        let s = (lpc * (lpc * (lpc * self.s1 + self.s2) + self.s3) + self.s4) * ml;
        let gg = lpc * lpc * lpc * lpc;
        (sample - self.r24 * s) / (1.0 + self.r24 * gg) + 1e-8
    }

    /// Process one sample. (Dexed runs this per host block; the per-sample
    /// form keeps the same coefficients and uses a fixed 256-sample
    /// crossfade when the filter engages or bypasses.)
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        // DC blocker.
        let y = x - self.dc_id + self.dc_r * self.dc_od;
        self.dc_id = x;
        self.dc_od = y;
        let x = y;

        if self.ui_cutoff != self.p_cutoff || self.ui_reso != self.p_reso {
            self.r_reso = 0.991 - logsc(1.0 - self.ui_reso, 0.0, 0.991);
            let cutoff_norm = logsc(self.ui_cutoff, 60.0, 19000.0);
            self.r_cutoff = (cutoff_norm * self.sr_inv * std::f32::consts::PI).tan();
            self.p_cutoff = self.ui_cutoff;
            self.p_reso = self.ui_reso;
        }

        let want_bypass = self.ui_cutoff >= 1.0;
        if self.fade_left == 0 {
            if want_bypass && self.bypassed {
                return x;
            }
            if self.bypassed {
                self.bypassed = false;
                self.fade = 0.0;
                self.fade_step = 1.0 / 256.0;
                self.fade_left = 256;
            } else if want_bypass && (self.s_cutoff - self.r_cutoff).abs() < 0.01 * self.r_cutoff {
                self.fade_step = -1.0 / 256.0;
                self.fade_left = 256;
            }
        }

        self.s_cutoff = self.r_cutoff + self.smooth * (self.s_cutoff - self.r_cutoff);
        self.s_reso = self.r_reso + self.smooth * (self.s_reso - self.r_reso);
        self.r24 = 3.5 * self.s_reso;
        let g = self.s_cutoff;
        let lpc = g / (1.0 + g);

        let dry = x;
        let mut s = dry;
        s -= 0.45 * tptlpupw(&mut self.c, s, 15.0, self.sr_inv);
        s = tptpc(&mut self.d, s, self.bright);
        let y0 = self.nr24(s, g, lpc);
        let v = (y0 - self.s1) * lpc;
        let res = v + self.s1;
        self.s1 = res + v;
        self.s1 = (self.s1 * self.rcor24).atan() * self.rcor24_inv;
        let y1 = res;
        let y2 = tptpc(&mut self.s2, y1, g);
        let y3 = tptpc(&mut self.s3, y2, g);
        let y4 = tptpc(&mut self.s4, y3, g);
        let out = y4 * (1.0 + self.r24 * 0.45);

        if self.fade_left > 0 {
            self.fade += self.fade_step;
            self.fade_left -= 1;
            if self.fade_left == 0 && self.fade_step < 0.0 {
                self.bypassed = true;
                self.fade = 1.0;
                return dry;
            }
        }
        let f = self.fade.clamp(0.0, 1.0);
        out * f + dry * (1.0 - f)
    }
}
