//! One DX7 voice (msfa / Dexed `Dx7Note`): six operators with their
//! envelopes, keyboard scaling, the pitch EG and the per-voice part of the
//! LFO and controller modulation.

use super::core::{Core, Kernel, OpParams, is_carrier};
use super::env::{Env, PitchEnv, scale_out_level};
use super::tables::{N, Rates};

const FEEDBACK_BITDEPTH: i32 = 8;

const COARSE_MUL: [i32; 32] = [
    -16777216, 0, 16777216, 26591258, 33554432, 38955489, 43368474, 47099600, 50331648, 53182516,
    55732705, 58039632, 60145690, 62083076, 63876816, 65546747, 67108864, 68576247, 69959732, 71268397,
    72509921, 73690858, 74816848, 75892776, 76922906, 77910978, 78860292, 79773775, 80654032, 81503396,
    82323963, 83117622,
];

const VELOCITY_DATA: [u8; 64] = [
    0, 70, 86, 97, 106, 114, 121, 126, 132, 138, 142, 148, 152, 156, 160, 163, 166, 170, 173, 174, 178,
    181, 184, 186, 189, 190, 194, 196, 198, 200, 202, 205, 206, 209, 211, 214, 216, 218, 220, 222, 224,
    225, 227, 229, 230, 232, 233, 235, 237, 238, 240, 241, 242, 243, 244, 246, 246, 248, 249, 250, 251,
    252, 253, 254,
];

const EXP_SCALE_DATA: [u8; 33] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 14, 16, 19, 23, 27, 33, 39, 47, 56, 66, 80, 94, 110, 126, 142, 158,
    174, 190, 206, 222, 238, 250,
];

const PITCHMODSENS_TAB: [i32; 8] = [0, 10, 20, 33, 55, 92, 153, 255];
const AMPMODSENS_TAB: [u32; 4] = [0, 4342338, 7171437, 16777216];

/// Q24 log2 frequency of MIDI note 0 at A4 = 440 Hz.
pub const LOGFREQ_BASE: i32 = 50857777;
pub const LOGFREQ_SEMI: i32 = (1 << 24) / 12;

pub fn midinote_to_logfreq(note: i32) -> i32 {
    LOGFREQ_BASE + LOGFREQ_SEMI * note
}

fn logfreq_round2semi(freq: i32) -> i32 {
    let rem = (freq - LOGFREQ_BASE) % LOGFREQ_SEMI;
    freq - rem
}

/// Operator frequency (Q24 log2 Hz) for a key. `detune_cents_offset` is an
/// extra per-unit analog-style offset (Vintage tolerance), normally 0.
pub fn osc_freq(midinote: i32, mode: i32, coarse: i32, fine: i32, detune: i32) -> i32 {
    if mode == 0 {
        let mut logfreq = midinote_to_logfreq(midinote);
        // Detune grows towards the low notes, as measured on a DX7.
        let detune_ratio = 0.0209 * (-0.396 * (f64::from(logfreq) / f64::from(1 << 24))).exp() / 7.0;
        logfreq = (f64::from(logfreq) + detune_ratio * f64::from(logfreq) * f64::from(detune - 7)) as i32;
        logfreq += COARSE_MUL[(coarse & 31) as usize];
        if fine != 0 {
            logfreq += (24204406.323123 * (1.0 + 0.01 * f64::from(fine)).ln() + 0.5).floor() as i32;
        }
        logfreq
    } else {
        // ((1 << 24) * log2(10) * .01) << 3
        let mut logfreq = (4458616 * ((coarse & 3) * 100 + fine)) >> 3;
        if detune > 7 {
            logfreq += 13457 * (detune - 7);
        }
        logfreq
    }
}

/// DX7 firmware coarse ratio table (log2, 4096 per octave; index 0 = x0.5).
const FW_COARSE: [i32; 32] = [
    -0x1000, 0, 0x1000, 0x195C, 0x2000, 0x2528, 0x295C, 0x2CEC, 0x3000, 0x32B8, 0x3528, 0x375A, 0x395C,
    0x3B34, 0x3CEC, 0x3E84, 0x4000, 0x4168, 0x42B8, 0x43F8, 0x4528, 0x4648, 0x475A, 0x4860, 0x495C,
    0x4A4C, 0x4B34, 0x4C14, 0x4CEC, 0x4DBA, 0x4E84, 0x4F44,
];
const FW_FIXED: [i32; 4] = [0, 0x3526, 0x6A4C, 0x9F74];
/// One EGS pitch step (1/1024 octave) in Q24.
pub const HW_STEP: i32 = 1 << 14;
const A4_LOGFREQ: i32 = LOGFREQ_BASE + 69 * LOGFREQ_SEMI;

/// Floor to the EGS pitch grid (anchored on A4).
#[inline]
pub fn hw_quantize(logfreq: i32) -> i32 {
    A4_LOGFREQ + (logfreq - A4_LOGFREQ).div_euclid(HW_STEP) * HW_STEP
}

/// Operator frequency the way the DX7 computes it (firmware v1.8 + EGS):
/// everything lives on a 1024-steps-per-octave grid (1.17 cents), keys
/// come from a 3-semitones-per-256-steps key code (so the 2nd and 3rd
/// semitone of each minor third are 0.39 / 0.78 cents flat), and ratios
/// are rounded log values, so 5:1 or 11:1 are not exactly harmonic.
/// Detune keeps the curve measured on a real DX7 (Dexed).
pub fn osc_freq_hw(midinote: i32, mode: i32, coarse: i32, fine: i32, detune: i32) -> i32 {
    if mode == 0 {
        let k = midinote.rem_euclid(12);
        let group = k / 3;
        let steps = group * 256 + [0, 85, 170][(k % 3) as usize];
        // Grid anchored on A4 = 440 Hz (A is the first key of its group).
        let oct = (midinote - k + 9 - 69) / 12;
        let mut logfreq = A4_LOGFREQ + oct * (1 << 24) + (steps - 768) * HW_STEP;
        let detune_ratio = 0.0209 * (-0.396 * (f64::from(logfreq) / f64::from(1 << 24))).exp() / 7.0;
        logfreq = (f64::from(logfreq) + detune_ratio * f64::from(logfreq) * f64::from(detune - 7)) as i32;
        let fine_units = (4096.0 * (1.0 + 0.01 * f64::from(fine)).log2()).round() as i32;
        let ratio_steps = (FW_COARSE[(coarse & 31) as usize] + fine_units).div_euclid(4);
        logfreq += ratio_steps * HW_STEP;
        hw_quantize(logfreq)
    } else {
        // 0x16AC>>2 = 1451 steps is 1.0067 Hz on the hardware.
        let units = (0x16AC + FW_FIXED[(coarse & 3) as usize] + fine * 136) >> 2;
        let base = (1.0067f64.log2() * f64::from(1 << 24)) as i32;
        let mut logfreq = base + (units - 1451) * HW_STEP;
        if detune != 7 {
            logfreq += (detune - 7) * HW_STEP;
        }
        logfreq
    }
}

fn scale_velocity(velocity: i32, sensitivity: i32) -> i32 {
    let v = velocity.clamp(0, 127);
    let vel_value = i32::from(VELOCITY_DATA[(v >> 1) as usize]) - 239;
    ((sensitivity * vel_value + 7) >> 3) << 4
}

fn scale_rate(midinote: i32, sensitivity: i32) -> i32 {
    let x = (midinote / 3 - 7).clamp(0, 31);
    (sensitivity * x) >> 3
}

fn scale_curve(group: i32, depth: i32, curve: i32) -> i32 {
    let mut scale = if curve == 0 || curve == 3 {
        (group * depth * 329) >> 12
    } else {
        let raw = i32::from(EXP_SCALE_DATA[group.clamp(0, 32) as usize]);
        (raw * depth * 329) >> 15
    };
    if curve < 2 {
        scale = -scale;
    }
    scale
}

fn scale_level(midinote: i32, break_pt: i32, left_depth: i32, right_depth: i32, left_curve: i32, right_curve: i32) -> i32 {
    let offset = midinote - break_pt - 17;
    if offset >= 0 {
        scale_curve((offset + 1) / 3, right_depth, right_curve)
    } else {
        scale_curve(-(offset - 1) / 3, left_depth, left_curve)
    }
}

/// One modulation source routed to pitch / amp / EG bias (Dexed `FmMod`).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct ModRoute {
    pub range: i32,
    pub pitch: bool,
    pub amp: bool,
    pub eg: bool,
}

/// MIDI controller state (Dexed `Controllers`).
#[derive(Clone, Copy)]
pub struct Controllers {
    /// 14-bit pitch bend, 0x2000 = centre.
    pub pitch_cc: i32,
    pub pitch_range_up: i32,
    pub pitch_range_dn: i32,
    pub pitch_step: i32,
    pub amp_mod: i32,
    pub pitch_mod: i32,
    pub eg_mod: i32,
    pub aftertouch_cc: i32,
    pub breath_cc: i32,
    pub foot_cc: i32,
    pub modwheel_cc: i32,
    pub portamento_cc: usize,
    pub portamento_gliss: bool,
    /// Q24 octave offset.
    pub master_tune: i32,
    pub wheel: ModRoute,
    pub foot: ModRoute,
    pub breath: ModRoute,
    pub at: ModRoute,
}

impl Default for Controllers {
    fn default() -> Self {
        let mut c = Self {
            pitch_cc: 0x2000,
            pitch_range_up: 2,
            pitch_range_dn: 2,
            pitch_step: 0,
            amp_mod: 0,
            pitch_mod: 0,
            eg_mod: 127,
            aftertouch_cc: 0,
            breath_cc: 0,
            foot_cc: 0,
            modwheel_cc: 0,
            portamento_cc: 0,
            portamento_gliss: false,
            master_tune: 0,
            wheel: ModRoute::default(),
            foot: ModRoute::default(),
            breath: ModRoute::default(),
            at: ModRoute::default(),
        };
        c.refresh();
        c
    }
}

impl Controllers {
    fn apply(&mut self, cc: i32, m: ModRoute) {
        let total = (cc as f32 * (0.01 * m.range as f32)) as i32;
        if m.amp {
            self.amp_mod = self.amp_mod.max(total);
        }
        if m.pitch {
            self.pitch_mod = self.pitch_mod.max(total);
        }
        if m.eg {
            self.eg_mod = self.eg_mod.max(total);
        }
    }

    pub fn refresh(&mut self) {
        self.amp_mod = 0;
        self.pitch_mod = 0;
        self.eg_mod = 0;
        self.apply(self.modwheel_cc, self.wheel);
        self.apply(self.breath_cc, self.breath);
        self.apply(self.foot_cc, self.foot);
        self.apply(self.aftertouch_cc, self.at);
        if !(self.wheel.eg || self.foot.eg || self.breath.eg || self.at.eg) {
            self.eg_mod = 127;
        }
    }

    pub fn porta_enabled(&self) -> bool {
        self.portamento_cc > 0
    }
}

#[derive(Clone, Copy, Default)]
pub struct Dx7Note {
    initialised: bool,
    env: [Env; 6],
    params: [OpParams; 6],
    pitchenv: PitchEnv,
    basepitch: [i32; 6],
    fb_buf: [i32; 2],
    fb_shift: i32,
    ampmodsens: [u32; 6],
    op_mode: [i32; 6],
    op_switch: u8,
    ampmoddepth: i32,
    algorithm: usize,
    pitchmoddepth: i32,
    pitchmodsens: i32,
    porta_curpitch: [i32; 6],
    /// Per-voice static pitch offset (Q24), used by the Vintage tolerance model.
    pub pitch_offset: i32,
    /// DX7 pitch grid and firmware frequency tables (Vintage).
    pub hw_pitch: bool,
}

impl Dx7Note {
    fn op_levels(patch: &[u8; 156], off: usize, midinote: i32, velocity: i32) -> ([i32; 4], [i32; 4], i32, i32) {
        let mut rates = [0; 4];
        let mut levels = [0; 4];
        for i in 0..4 {
            rates[i] = i32::from(patch[off + i]);
            levels[i] = i32::from(patch[off + 4 + i]);
        }
        let mut outlevel = scale_out_level(i32::from(patch[off + 16]));
        outlevel += scale_level(
            midinote,
            i32::from(patch[off + 8]),
            i32::from(patch[off + 9]),
            i32::from(patch[off + 10]),
            i32::from(patch[off + 11]),
            i32::from(patch[off + 12]),
        );
        outlevel = outlevel.min(127) << 5;
        outlevel += scale_velocity(velocity, i32::from(patch[off + 15]));
        outlevel = outlevel.max(0);
        let rate_scaling = scale_rate(midinote, i32::from(patch[off + 13]));
        (rates, levels, outlevel, rate_scaling)
    }

    fn voice_globals(&mut self, patch: &[u8; 156]) {
        self.algorithm = usize::from(patch[134] & 31);
        let feedback = i32::from(patch[135]);
        self.fb_shift = if feedback != 0 { FEEDBACK_BITDEPTH - feedback } else { 16 };
        self.pitchmoddepth = (i32::from(patch[139]) * 165) >> 6;
        self.pitchmodsens = PITCHMODSENS_TAB[usize::from(patch[143] & 7)];
        self.ampmoddepth = (i32::from(patch[140]) * 165) >> 6;
        self.op_switch = patch[155];
    }

    fn freq_of(&self, midinote: i32, mode: i32, patch: &[u8; 156], off: usize) -> i32 {
        let (c, f, d) = (i32::from(patch[off + 18]), i32::from(patch[off + 19]), i32::from(patch[off + 20]));
        if self.hw_pitch { osc_freq_hw(midinote, mode, c, f, d) } else { osc_freq(midinote, mode, c, f, d) }
    }

    pub fn init(&mut self, r: &Rates, patch: &[u8; 156], midinote: i32, velocity: i32) {
        self.initialised = true;
        for op in 0..6 {
            let off = op * 21;
            let (rates, levels, outlevel, rs) = Self::op_levels(patch, off, midinote, velocity);
            self.env[op].init(r, rates, levels, outlevel, rs);
            let mode = i32::from(patch[off + 17]);
            let freq = self.freq_of(midinote, mode, patch, off);
            self.op_mode[op] = mode;
            self.basepitch[op] = freq;
            self.porta_curpitch[op] = freq;
            self.ampmodsens[op] = AMPMODSENS_TAB[usize::from(patch[off + 14] & 3)];
        }
        let mut rates = [0; 4];
        let mut levels = [0; 4];
        for i in 0..4 {
            rates[i] = i32::from(patch[126 + i]);
            levels[i] = i32::from(patch[130 + i]);
        }
        self.pitchenv.set(r, rates, levels);
        self.voice_globals(patch);
    }

    /// Re-apply an edited patch to a sounding note.
    pub fn update(&mut self, patch: &[u8; 156], midinote: i32, velocity: i32) {
        for op in 0..6 {
            let off = op * 21;
            let mode = i32::from(patch[off + 17]);
            self.basepitch[op] = self.freq_of(midinote, mode, patch, off);
            self.ampmodsens[op] = AMPMODSENS_TAB[usize::from(patch[off + 14] & 3)];
            self.op_mode[op] = mode;
            let (rates, levels, outlevel, rs) = Self::op_levels(patch, off, midinote, velocity);
            self.env[op].update(rates, levels, outlevel, rs);
        }
        self.voice_globals(patch);
    }

    pub fn init_portamento(&mut self, src: &Dx7Note) {
        self.porta_curpitch = src.porta_curpitch;
    }

    /// Render one block into `buf` (overwritten).
    pub fn compute(&mut self, r: &Rates, core: &mut Core, kernel: Kernel, buf: &mut [i32; N], lfo_val: i32, lfo_delay: i32, ctrls: &Controllers, carrier_norm: bool) {
        let hw = self.hw_pitch;
        let q = move |x: i32| if hw { hw_quantize(x) } else { x };
        // ---- pitch ----
        let pmd = (self.pitchmoddepth as u32).wrapping_mul(lfo_delay as u32);
        let senslfo = self.pitchmodsens * (lfo_val - (1 << 23));
        let pmod_1 = ((i64::from(pmd) * i64::from(senslfo)) >> 39).abs() as i32;
        let pmod_2 = ((i64::from(ctrls.pitch_mod) * i64::from(senslfo)) >> 14).abs() as i32;
        let mut pitch_mod = pmod_1.max(pmod_2);
        pitch_mod = self.pitchenv.getsample() + if senslfo < 0 { -pitch_mod } else { pitch_mod };

        let mut pb = ctrls.pitch_cc - 0x2000;
        if pb != 0 {
            if ctrls.pitch_step == 0 {
                let range = if pb >= 0 { ctrls.pitch_range_up } else { ctrls.pitch_range_dn };
                pb = ((pb << 11) as f32 * range as f32 / 12.0) as i32;
            } else {
                let stp = 12 / ctrls.pitch_step;
                pb = pb * stp / 8191;
                pb = (pb * (8191 / stp)) << 11;
            }
        }
        let pitch_base = pb + ctrls.master_tune + self.pitch_offset;
        pitch_mod += pitch_base;

        // ---- amp mod ----
        let lfo_inv = (1 << 24) - lfo_val;
        let amod_1 = ((i64::from(self.ampmoddepth) * i64::from(lfo_delay)) >> 8) as u32;
        let amod_1 = ((i64::from(amod_1) * i64::from(lfo_inv)) >> 24) as u32;
        let amod_2 = ((i64::from(ctrls.amp_mod) * i64::from(lfo_inv)) >> 7) as u32;
        let mut amd_mod = amod_1.max(amod_2);
        let amod_3 = ((ctrls.eg_mod + 1) << 17) as u32;
        amd_mod = amd_mod.max((1u32 << 24).saturating_sub(amod_3));

        let porta_rate = if ctrls.porta_enabled() {
            let i = ctrls.portamento_cc.min(127);
            if ctrls.portamento_gliss { r.porta_gliss[i] } else { r.porta[i] }
        } else {
            r.porta[0]
        };

        for op in 0..6 {
            if self.op_switch & (1 << op) == 0 {
                self.env[op].getsample();
                self.params[op].level_in = 0;
                continue;
            }
            let mut basepitch = self.basepitch[op];
            if self.op_mode[op] != 0 {
                self.params[op].freq = r.freq.lookup(q(basepitch + pitch_base));
            } else {
                if self.porta_curpitch[op] != self.basepitch[op] {
                    basepitch = self.porta_curpitch[op];
                    if ctrls.portamento_gliss {
                        basepitch = logfreq_round2semi(basepitch);
                    }
                    let cur = self.porta_curpitch[op];
                    let dst = self.basepitch[op];
                    let up = cur < dst;
                    let mut np = if up { cur + porta_rate } else { cur - porta_rate };
                    if (up && np > dst) || (!up && np < dst) {
                        np = dst;
                    }
                    self.porta_curpitch[op] = np;
                }
                self.params[op].freq = r.freq.lookup(q(basepitch + pitch_mod));
            }
            let mut level = self.env[op].getsample();
            if self.ampmodsens[op] != 0 {
                let sensamp = ((u64::from(amd_mod) * u64::from(self.ampmodsens[op])) >> 24) as u32;
                let pt = ((sensamp as f32) / 262144.0 * 0.07 + 12.2).exp() as u32;
                let ldiff = ((level as u64 * (u64::from(pt) << 4)) >> 28) as i32;
                level -= ldiff;
            }
            self.params[op].level_in = level;
        }
        core.render(kernel, buf, &mut self.params, self.algorithm, &mut self.fb_buf, self.fb_shift, carrier_norm);
    }

    pub fn keyup(&mut self) {
        for e in &mut self.env {
            e.keydown(false);
        }
        self.pitchenv.keydown(false);
    }

    /// Mono mode: take over envelopes and signal of another note.
    pub fn transfer_state(&mut self, src: &Dx7Note) {
        for i in 0..6 {
            self.env[i].transfer(&src.env[i]);
            self.params[i].gain_out = src.params[i].gain_out;
            self.params[i].phase = src.params[i].phase;
        }
    }

    pub fn transfer_signal(&mut self, src: &Dx7Note) {
        for i in 0..6 {
            self.params[i].gain_out = src.params[i].gain_out;
            self.params[i].phase = src.params[i].phase;
        }
    }

    pub fn transfer_phase(&mut self, src: &Dx7Note) {
        for i in 0..6 {
            self.params[i].phase = src.params[i].phase;
        }
    }

    pub fn osc_sync(&mut self) {
        for p in &mut self.params {
            p.gain_out = 0;
            p.phase = 0;
        }
    }

    pub fn is_playing(&self) -> bool {
        self.initialised && (0..6).any(|i| is_carrier(self.algorithm, i) && self.env[i].is_active())
    }

    /// Envelope stage of each operator (for the UI).
    pub fn stages(&self) -> [usize; 6] {
        std::array::from_fn(|i| self.env[i].stage())
    }
}
