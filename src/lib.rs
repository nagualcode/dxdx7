//! DXDX7 - a Yamaha DX7 for modern DAWs, a Rust port of Dexed built with
//! truce.
//!
//! Plugin layer: host parameters, MIDI and the editor. The sound engine is
//! in [`dx7`]; cartridge and voice SysEx handling in [`sysex`].
//!
//! - Every byte of a DX7 voice is a host parameter with a fixed id (see
//!   `tools/gen_params.py`), so voices are automatable and DAW projects
//!   recall edits exactly. The cartridge, program number and voice name are
//!   saved alongside as persistent state.
//! - Notes, pitch bend, mod wheel (CC1), breath (CC2), foot (CC4), sustain
//!   (CC64) and channel pressure come from the host MIDI input. MIDI program
//!   changes select a voice of the current cartridge; a single-voice DX7
//!   SysEx dump (VCED) received over MIDI loads that voice.

use std::sync::Arc;

use truce::prelude::*;

pub mod cartridges;
pub mod dx7;
pub mod enums;
mod params;
pub mod state;
pub mod sysex;
mod ui;

pub use params::{DxParams, OP_ON_IDS, VOICE_PARAM_IDS, VOICE_PARAM_MAX, op_param_id, voice_param_id};

use dx7::note::ModRoute;
use dx7::{Engine, Settings};
use params::DxParamsParamId as P;

// ---------------------------------------------------------------------------
// Value formatting (host automation lanes and the editor)
// ---------------------------------------------------------------------------

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Yamaha octave numbering: middle C (MIDI 60) = C3.
pub fn note_name(midi: i32) -> String {
    format!("{}{}", NOTE_NAMES[midi.rem_euclid(12) as usize], midi.div_euclid(12) - 2)
}

impl DxParams {
    fn fmt_breakpoint(&self, v: f64) -> String {
        note_name(v.round() as i32 + 21)
    }
    fn fmt_coarse(&self, v: f64) -> String {
        let c = v.round() as i32;
        if c == 0 { "0.50".into() } else { format!("{c}") }
    }
    fn fmt_detune(&self, v: f64) -> String {
        format!("{:+}", v.round() as i32 - 7)
    }
    fn fmt_peg_level(&self, v: f64) -> String {
        let t = dx7::env::PITCHENV_TAB[(v.round() as usize).min(99)];
        format!("{:+.1} st", f64::from(t) * 12.0 / 32.0)
    }
    fn fmt_algorithm(&self, v: f64) -> String {
        format!("{}", v.round() as i32 + 1)
    }
    fn fmt_lfo_speed(&self, v: f64) -> String {
        format!("{:.2} Hz", dx7::lfo::LFO_HZ[(v.round() as usize).min(99)])
    }
    fn fmt_transpose(&self, v: f64) -> String {
        note_name(v.round() as i32 + 36)
    }
    fn fmt_cutoff(&self, v: f64) -> String {
        if v >= 1.0 {
            return "Off".into();
        }
        let hz = dx7::fx::cutoff_hz(v as f32);
        if hz >= 1000.0 { format!("{:.1} kHz", hz / 1000.0) } else { format!("{hz:.0} Hz") }
    }
    fn fmt_percent(&self, v: f64) -> String {
        format!("{:.0}%", v * 100.0)
    }
    fn fmt_cents(&self, v: f64) -> String {
        format!("{v:+.0} ct")
    }
    fn fmt_bend_step(&self, v: f64) -> String {
        if v < 0.5 { "Off".into() } else { format!("{} st", v.round() as i32) }
    }
}

/// Operator frequency readout: "1.00", "3.51" or "440 Hz" (fixed mode).
pub fn op_freq_label(mode: u8, coarse: u8, fine: u8) -> String {
    if mode == 0 {
        let c = if coarse == 0 { 0.5 } else { f64::from(coarse) };
        format!("{:.2}", c * (1.0 + f64::from(fine) / 100.0))
    } else {
        let hz = 10f64.powf(f64::from(coarse & 3) + f64::from(fine) / 100.0);
        if hz >= 1000.0 { format!("{:.2}k", hz / 1000.0) } else { format!("{hz:.1}Hz") }
    }
}

fn route(range: &IntParam, pitch: &BoolParam, amp: &BoolParam, eg: &BoolParam) -> ModRoute {
    ModRoute { range: range.value() as i32, pitch: pitch.value(), amp: amp.value(), eg: eg.value() }
}

fn read_settings(p: &DxParams) -> Settings {
    Settings {
        engine: p.engine.value(),
        polyphony: p.polyphony.value() as usize,
        mono: p.mono.value(),
        cutoff: p.cutoff.value() as f32,
        reso: p.resonance.value() as f32,
        master_tune: p.master_tune.value() as f32,
        bend_up: p.bend_up.value() as i32,
        bend_down: p.bend_down.value() as i32,
        bend_step: p.bend_step.value() as i32,
        porta_time: p.porta_time.value() as i32,
        glissando: p.glissando.value(),
        wheel: route(&p.wheel_range, &p.wheel_pitch, &p.wheel_amp, &p.wheel_eg),
        foot: route(&p.foot_range, &p.foot_pitch, &p.foot_amp, &p.foot_eg),
        breath: route(&p.breath_range, &p.breath_pitch, &p.breath_amp, &p.breath_eg),
        at: route(&p.at_range, &p.at_pitch, &p.at_amp, &p.at_eg),
        vel_dx7: p.vel_curve.index() == 1,
        dac: p.dac.value(),
        out_filter: p.out_filter.value(),
        noise: p.analog_noise.value() as f32,
        drive: p.drive.value() as f32,
        tolerance: p.tolerance.value() as f32,
        hw_pitch: p.hw_pitch.value(),
        carrier_norm: p.carrier_norm.value(),
        lfo_steps: p.lfo_steps.value(),
    }
}

/// Write a voice into the parameter store directly (audio thread: MIDI
/// program change / SysEx). The editor uses host gestures instead.
fn store_voice(p: &DxParams, v: &sysex::Voice) {
    for (b, (&id, &max)) in VOICE_PARAM_IDS.iter().zip(VOICE_PARAM_MAX.iter()).enumerate() {
        p.set_normalized(id, f64::from(v[b].min(max)) / f64::from(max));
    }
    for (k, &id) in OP_ON_IDS.iter().enumerate() {
        // OP1 is slot 5 (bit 5).
        let on = v[155] & (1 << (5 - k)) != 0;
        p.set_normalized(id, if on { 1.0 } else { 0.0 });
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// This many voice bytes changing in one block counts as a preset change.
const PRESET_CHANGE_BYTES: usize = 8;

pub struct DxDx7 {
    engine: Box<Engine>,
    voice: sysex::Voice,
    peak: [f32; 2],
    sysex_buf: Vec<u8>,
}

impl Default for DxDx7 {
    fn default() -> Self {
        Self { engine: Box::default(), voice: [0; 156], peak: [0.0; 2], sysex_buf: Vec::with_capacity(4200) }
    }
}

impl DxDx7 {
    fn sync(&mut self, params: &DxParams) {
        let mut v = self.voice;
        params.read_voice(&mut v);
        if v[..145] != self.voice[..145] || v[155] != self.voice[155] {
            // A new preset (program change, cartridge browser, LOAD, INIT)
            // rewrites many bytes at once; a knob moves one. Silence the old
            // sound on preset changes so nothing keeps ringing.
            let changed = v[..145].iter().zip(&self.voice[..145]).filter(|(a, b)| a != b).count();
            if changed >= PRESET_CHANGE_BYTES {
                self.engine.reset_voices();
            }
            self.voice = v;
            self.engine.set_patch(&v);
        }
        self.engine.apply_settings(&read_settings(params), false);
    }

    fn program_change(&mut self, params: &DxParams, program: u8) {
        let prog = usize::from(program & 31);
        // Never block the audio thread on the editor.
        let Ok(mut st) = params.patch.try_write() else { return };
        if st.cart.len() != sysex::CART_SIZE {
            return;
        }
        let mut cart = sysex::Cartridge { data: [0; sysex::CART_SIZE] };
        cart.data.copy_from_slice(&st.cart);
        let v = cart.voice(prog);
        st.program = prog as u32;
        st.voice_name.clear();
        st.voice_name.extend(sysex::normalize_name(&v[145..155]).chars());
        drop(st);
        store_voice(params, &v);
    }

    fn handle_event(&mut self, params: &DxParams, events: &EventList, body: &EventBody) {
        match *body {
            EventBody::NoteOn { note, velocity, .. } => self.engine.note_on(note, velocity),
            EventBody::NoteOff { note, .. } => self.engine.note_off(note),
            EventBody::PitchBend { value, .. } => self.engine.pitch_bend(value),
            EventBody::ControlChange { cc, value, .. } => self.engine.control_change(cc, value),
            EventBody::ChannelPressure { pressure, .. } => self.engine.channel_pressure(pressure),
            EventBody::ProgramChange { program, .. } => self.program_change(params, program),
            EventBody::SysEx { .. } => {
                let bytes = events.sysex_bytes(body);
                self.sysex_buf.clear();
                if bytes.first() != Some(&0xF0) {
                    self.sysex_buf.push(0xF0);
                }
                self.sysex_buf.extend_from_slice(bytes);
                if let Ok(sysex::Loaded::Voice(v)) = sysex::load(&self.sysex_buf) {
                    store_voice(params, &v);
                    if let Ok(mut st) = params.patch.try_write() {
                        st.voice_name.clear();
                        st.voice_name.extend(sysex::normalize_name(&v[145..155]).chars());
                    }
                }
            }
            _ => {}
        }
    }
}

impl PluginLogic for DxDx7 {
    type Params = DxParams;
    type DspState = Self;

    fn bus_layouts() -> Vec<BusLayout> {
        vec![BusLayout::new().with_output("Main", ChannelConfig::Stereo)]
    }

    fn reset(state: &mut Self, params: &DxParams, config: &AudioConfig) {
        state.engine.reset(config.sample_rate);
        state.voice = [0; 156];
        state.sync(params);
    }

    fn process(state: &mut Self, params: &DxParams, buffer: &mut AudioBuffer, events: &EventList, context: &mut ProcessContext) -> ProcessStatus {
        state.sync(params);
        let n = buffer.num_samples();
        let channels = buffer.num_output_channels();
        let mut next = 0;
        let mut peak = 0.0_f32;
        for i in 0..n {
            while let Some(ev) = events.get(next) {
                if ev.sample_offset as usize > i {
                    break;
                }
                state.handle_event(params, events, &ev.body);
                next += 1;
            }
            let gain = db_to_linear(params.output.read());
            let y = state.engine.process_sample() * gain;
            peak = peak.max(y.abs());
            for ch in 0..channels {
                buffer.output(ch)[i] = y;
            }
        }
        while let Some(ev) = events.get(next) {
            state.handle_event(params, events, &ev.body);
            next += 1;
        }
        // A program change or SysEx may have rewritten the voice.
        state.sync(params);

        let decay = (-(n as f64) / (0.3 * context.sample_rate)).exp() as f32;
        for s in state.peak.iter_mut() {
            *s = peak.max(*s * decay);
        }
        context.set_meter(P::MeterL, state.peak[0]);
        context.set_meter(P::MeterR, state.peak[1]);
        ProcessStatus::Normal
    }

    fn editor(params: Arc<DxParams>) -> Box<dyn Editor> {
        ui::editor(params)
    }
}

truce::plugin! {
    logic: DxDx7,
    params: DxParams,
}

truce::enable_rt_paranoid!();

#[cfg(test)]
mod tests;
