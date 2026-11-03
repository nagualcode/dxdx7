//! The DX7 sound engine.
//!
//! `Synth` is a Rust port of the Dexed engine (msfa by Raph Levien, Dexed
//! by Pascal Gauthier): voices, envelopes, LFO and the FM core. `Engine`
//! wraps it with the output path selected by the Engine switch:
//!
//! - Modern / Mark I: the core runs at the host rate and each voice is
//!   clipped to 16 bits and summed in floating point, exactly like Dexed.
//! - DX7 Vintage: the Mark I core runs at the DX7's own 49.1 kHz, all voices
//!   are summed as integers like the OPS accumulator, then the DX7 DAC,
//!   its sample-and-hold, the reconstruction filter and the analog output
//!   stage are modelled (see `vintage.rs`, `docs/HARDWARE.md`).

pub mod core;
pub mod env;
pub mod fx;
pub mod lfo;
pub mod note;
pub mod resample;
pub mod synth;
pub mod tables;
pub mod vintage;

use crate::enums::{DacSw, EngineSw};
use core::Kernel;
use fx::PluginFx;
use note::ModRoute;
use resample::Resampler;
use synth::{Block, HwModel, Synth};
use tables::N;
use vintage::{Analog, DX7_SAMPLE_RATE};

/// Plugin-level settings pushed from the host parameters each block.
#[derive(Clone, Copy, PartialEq)]
pub struct Settings {
    pub engine: EngineSw,
    pub polyphony: usize,
    pub mono: bool,
    pub cutoff: f32,
    pub reso: f32,
    /// Cents.
    pub master_tune: f32,
    pub bend_up: i32,
    pub bend_down: i32,
    pub bend_step: i32,
    pub porta_time: i32,
    pub glissando: bool,
    pub wheel: ModRoute,
    pub foot: ModRoute,
    pub breath: ModRoute,
    pub at: ModRoute,
    pub vel_dx7: bool,
    pub dac: DacSw,
    pub out_filter: bool,
    pub noise: f32,
    pub drive: f32,
    pub tolerance: f32,
    pub hw_pitch: bool,
    pub carrier_norm: bool,
    pub lfo_steps: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: EngineSw::Vintage,
            polyphony: 16,
            mono: false,
            cutoff: 1.0,
            reso: 0.0,
            master_tune: 0.0,
            bend_up: 2,
            bend_down: 2,
            bend_step: 0,
            porta_time: 0,
            glissando: false,
            wheel: ModRoute { range: 50, pitch: true, amp: false, eg: false },
            foot: ModRoute::default(),
            breath: ModRoute::default(),
            at: ModRoute::default(),
            vel_dx7: false,
            dac: DacSw::Dx7,
            out_filter: true,
            noise: 1.0,
            drive: 0.0,
            tolerance: 0.0,
            hw_pitch: true,
            carrier_norm: true,
            lfo_steps: true,
        }
    }
}

/// 15-bit DAC units to float. A full-scale voice (16384) lands at 0.25.
/// With the DX7's carrier scaling a voice never exceeds that, whatever the
/// algorithm; 0.25 makes a typical two-carrier voice as loud as in
/// Mark I / Dexed, so switching engines doesn't jump in level.
const VINT_SCALE: f32 = 0.25 / 16384.0;

/// Fade-out before voices are freed on a preset change (no click).
const RESET_FADE_SECS: f64 = 0.005;

pub struct Engine {
    host_sr: f64,
    settings: Settings,
    synth: Synth,
    block: Block,
    pos: usize,
    fx: PluginFx,
    resampler: Resampler,
    analog: Analog,
    /// Samples left in a voice reset fade-out (0 = none running).
    fade: u32,
    fade_len: u32,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(44100.0)
    }
}

impl Engine {
    pub fn new(host_sr: f64) -> Self {
        // Build the shared lookup tables now, not on the audio thread.
        let _ = tables::tables();
        let settings = Settings::default();
        let (core_sr, kernel) = Self::core_for(settings.engine, host_sr);
        Self {
            host_sr,
            settings,
            synth: Synth::new(core_sr, kernel),
            block: Block { mix: [0.0; N], vint: [0.0; N] },
            pos: N,
            fx: PluginFx::new(host_sr as f32),
            resampler: Resampler::new(DX7_SAMPLE_RATE, host_sr, vintage::output_filter_response),
            analog: Analog::new(host_sr),
            fade: 0,
            fade_len: ((host_sr * RESET_FADE_SECS) as u32).max(1),
        }
    }

    fn core_for(engine: EngineSw, host_sr: f64) -> (f64, Kernel) {
        match engine {
            EngineSw::Modern => (host_sr, Kernel::Modern),
            EngineSw::MarkI => (host_sr, Kernel::MarkI),
            EngineSw::Vintage => (DX7_SAMPLE_RATE, Kernel::MarkI),
        }
    }

    /// Rebuild everything for a new host sample rate (all notes stop).
    pub fn reset(&mut self, host_sr: f64) {
        let patch = self.synth.patch;
        let settings = self.settings;
        *self = Self::new(host_sr);
        self.synth.set_patch(&patch);
        self.settings.engine = EngineSw::Modern; // force re-apply below
        self.apply_settings(&settings, true);
    }

    pub fn set_patch(&mut self, patch: &[u8; 156]) {
        self.synth.set_patch(patch);
    }

    pub fn patch(&self) -> &[u8; 156] {
        &self.synth.patch
    }

    pub fn apply_settings(&mut self, s: &Settings, force: bool) {
        if !force && *s == self.settings {
            return;
        }
        if s.engine != self.settings.engine || force {
            let (core_sr, kernel) = Self::core_for(s.engine, self.host_sr);
            if (core_sr - self.synth.rates.sample_rate).abs() > 0.5 || force {
                self.synth.set_rate(core_sr, kernel);
                self.pos = N;
            }
            self.synth.kernel = kernel;
        }
        self.synth.set_polyphony(s.polyphony);
        self.synth.set_mono(s.mono);
        let c = &mut self.synth.ctrls;
        c.pitch_range_up = s.bend_up;
        c.pitch_range_dn = s.bend_down;
        c.pitch_step = s.bend_step;
        c.portamento_cc = (s.porta_time.clamp(0, 99) as f32 * 127.0 / 99.0) as usize;
        c.portamento_gliss = s.glissando;
        c.master_tune = (s.master_tune / 1200.0 * (1 << 24) as f32) as i32;
        c.wheel = s.wheel;
        c.foot = s.foot;
        c.breath = s.breath;
        c.at = s.at;
        c.refresh();
        self.fx.ui_cutoff = s.cutoff;
        self.fx.ui_reso = s.reso;
        let vintage = s.engine == EngineSw::Vintage;
        let mut dac_gain = self.synth.hw.dac_gain;
        if s.tolerance != self.settings.tolerance || force {
            // 0.1 % ladder resistors: up to +-0.2 % per range at full tolerance.
            dac_gain = [1.0, 1.0 + 0.0021 * s.tolerance, 1.0 - 0.0017 * s.tolerance, 1.0 + 0.0013 * s.tolerance];
        }
        let hw = if vintage {
            HwModel { dac: s.dac, dac_gain, carrier_norm: s.carrier_norm, hw_pitch: s.hw_pitch, lfo_steps: s.lfo_steps }
        } else {
            HwModel { dac_gain, ..HwModel::OFF }
        };
        self.synth.hw = hw;
        self.resampler.filter = s.out_filter;
        self.analog.noise = s.noise;
        self.analog.drive = s.drive;
        if s.tolerance != self.settings.tolerance || force {
            self.set_tolerance(s.tolerance);
        }
        self.settings = *s;
    }

    /// Per-voice pitch offsets: each of the 16 voice slots of a real unit
    /// shares one master clock, so voices don't drift - but people often
    /// associate vintage units with slight detuning from aged/replaced
    /// crystals. This only adds a fixed, tiny per-slot offset (<= +-1.5
    /// cents at full) and a global offset; 0 = digitally exact.
    fn set_tolerance(&mut self, amount: f32) {
        let mut seed: u32 = 0x9E37_79B9;
        for o in self.synth.voice_pitch_offset.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let r = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
            let cents = amount * 1.5 * r;
            *o = (cents / 1200.0 * (1 << 24) as f32) as i32;
        }
    }

    // ---------------- MIDI ----------------

    /// Stop every voice for a preset change: a short fade-out, then all
    /// voices are freed (sustain included). Notes played after the fade use
    /// the new preset.
    pub fn reset_voices(&mut self) {
        if self.synth.active_voices() == 0 {
            self.synth.panic();
        } else if self.fade == 0 {
            self.fade = self.fade_len;
        }
    }

    /// Finish a running reset fade at once.
    fn finish_fade(&mut self) {
        if self.fade > 0 {
            self.fade = 0;
            self.cut_voices();
        }
    }

    /// Free all voices and drop the audio already rendered from them (the
    /// rest of the current block and the resampler history), so nothing of
    /// the old sound plays after the fade.
    fn cut_voices(&mut self) {
        self.synth.panic();
        self.block.mix = [0.0; N];
        self.block.vint = [0.0; N];
        self.resampler.clear();
    }

    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.finish_fade();
        let v = if self.settings.vel_dx7 { (f32::from(velocity) * 0.787_401_6) as u8 } else { velocity };
        self.synth.note_on(note, v.max(1));
    }

    pub fn note_off(&mut self, note: u8) {
        self.synth.note_off(note);
    }

    /// 14-bit pitch bend, 8192 = centre.
    pub fn pitch_bend(&mut self, value: u16) {
        self.synth.ctrls.pitch_cc = i32::from(value.min(16383));
    }

    pub fn control_change(&mut self, cc: u8, value: u8) {
        let v = i32::from(value);
        let c = &mut self.synth.ctrls;
        match cc {
            1 => c.modwheel_cc = v,
            2 => c.breath_cc = v,
            4 => c.foot_cc = v,
            64 => {
                self.synth.set_sustain(value >= 64);
                return;
            }
            120 | 123 => {
                self.synth.panic();
                return;
            }
            _ => return,
        }
        c.refresh();
    }

    pub fn channel_pressure(&mut self, value: u8) {
        self.synth.ctrls.aftertouch_cc = i32::from(value);
        self.synth.ctrls.refresh();
    }

    pub fn all_notes_off(&mut self) {
        self.synth.panic();
    }

    pub fn active_voices(&self) -> usize {
        self.synth.active_voices()
    }

    // ---------------- audio ----------------

    #[inline]
    fn next_core_mix(&mut self) -> f32 {
        if self.pos >= N {
            self.synth.render(&mut self.block);
            self.pos = 0;
        }
        let x = self.block.mix[self.pos];
        self.pos += 1;
        x
    }

    #[inline]
    pub fn process_sample(&mut self) -> f32 {
        let y = match self.settings.engine {
            EngineSw::Modern | EngineSw::MarkI => self.next_core_mix(),
            EngineSw::Vintage => {
                let Self { resampler, synth, block, pos, .. } = self;
                let x = resampler.next(|| {
                    if *pos >= N {
                        synth.render(block);
                        *pos = 0;
                    }
                    let v = block.vint[*pos] * VINT_SCALE;
                    *pos += 1;
                    v
                });
                self.analog.process(x)
            }
        };
        let y = self.fx.process(y);
        if self.fade > 0 {
            let g = self.fade as f32 / self.fade_len as f32;
            self.fade -= 1;
            if self.fade == 0 {
                self.cut_voices();
            }
            return y * g * g;
        }
        y
    }
}
