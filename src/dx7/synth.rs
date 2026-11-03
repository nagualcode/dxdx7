//! Voice allocation and MIDI handling (Dexed `DexedAudioProcessor`
//! keydown/keyup/processBlock), running at the core sample rate in
//! blocks of N samples.

use super::core::{Core, Kernel};
use super::lfo::Lfo;
use super::note::{Controllers, Dx7Note};
use super::tables::{N, Rates};
use crate::enums::DacSw;

pub const MAX_VOICES: usize = 16;

#[derive(Clone, Copy, Default)]
struct Voice {
    /// The MIDI key as played, before the patch transpose. Note-off is
    /// matched on this, so a preset with another transpose cannot strand
    /// a held note.
    key: i32,
    /// The note that sounds (key + transpose).
    midi_note: i32,
    velocity: i32,
    keydown: bool,
    sustained: bool,
    live: bool,
    keydown_seq: u64,
    note: Dx7Note,
}

pub struct Synth {
    pub rates: Rates,
    core: Core,
    pub kernel: Kernel,
    voices: Box<[Voice; MAX_VOICES]>,
    pub lfo: Lfo,
    pub ctrls: Controllers,
    pub patch: [u8; 156],
    pub polyphony: usize,
    pub mono: bool,
    pub sustain: bool,
    current: usize,
    next_seq: u64,
    last_active: usize,
    voice_buf: [i32; N],
    /// Vintage: per-voice static pitch offsets (Q24), from unit tolerance.
    pub voice_pitch_offset: [i32; MAX_VOICES],
    /// Vintage hardware switches.
    pub hw: HwModel,
    lfo_hold: (i32, i32),
    lfo_tick: u32,
}

/// Which parts of the DX7 hardware behaviour are modelled (Vintage engine).
#[derive(Clone, Copy, PartialEq)]
pub struct HwModel {
    pub dac: DacSw,
    /// Gain error of the DAC's /2, /4, /8 analog ranges (1.0 = exact).
    pub dac_gain: [f32; 4],
    pub carrier_norm: bool,
    pub hw_pitch: bool,
    /// LFO / modulation updated at the firmware's ~372 Hz instead of per block.
    pub lfo_steps: bool,
}

impl HwModel {
    pub const OFF: HwModel = HwModel { dac: DacSw::Clean, dac_gain: [1.0; 4], carrier_norm: false, hw_pitch: false, lfo_steps: false };
}

/// DX7 per-voice converter (Shirriff, YM21280 part V; VDX7): the 15-bit
/// voice sample goes to a 12-bit DAC shifted by 0..3 bits and an analog
/// /1../8 ladder brings it back - sign + 11-bit mantissa + 2-bit exponent.
/// Returns (quantised value, range index 0 = loudest).
#[inline]
pub fn dx7_dac(v: i32) -> (i32, usize) {
    let m = v.unsigned_abs() as i32;
    let (mask, r) = if m >= 4096 {
        (!7, 0)
    } else if m >= 2048 {
        (!3, 1)
    } else if m >= 1024 {
        (!1, 2)
    } else {
        (!0, 3)
    };
    let q = m & mask;
    (if v < 0 { -q } else { q }, r)
}

/// One rendered block, before any output-stage processing.
pub struct Block {
    /// Dexed's per-voice-clipped float mix (Modern / Mark I path).
    pub mix: [f32; N],
    /// Vintage path: sum of the voices after each voice's own DAC
    /// conversion, in 15-bit DAC units (one full carrier = 16384).
    pub vint: [f32; N],
}

impl Synth {
    pub fn new(sample_rate: f64, kernel: Kernel) -> Self {
        let mut v = Voice::default();
        v.midi_note = -1;
        let mut s = Self {
            rates: Rates::new(sample_rate),
            core: Core::default(),
            kernel,
            voices: Box::new([v; MAX_VOICES]),
            lfo: Lfo::default(),
            ctrls: Controllers::default(),
            patch: crate::sysex::init_voice(),
            polyphony: MAX_VOICES,
            mono: false,
            sustain: false,
            current: 0,
            next_seq: 0,
            last_active: 0,
            voice_buf: [0; N],
            voice_pitch_offset: [0; MAX_VOICES],
            hw: HwModel::OFF,
            lfo_hold: (1 << 23, 0),
            lfo_tick: 0,
        };
        s.lfo.reset(&s.rates, &s.patch[137..143]);
        s
    }

    /// Change the core sample rate / kernel in place (no allocation; all
    /// notes stop).
    pub fn set_rate(&mut self, sample_rate: f64, kernel: Kernel) {
        if (sample_rate - self.rates.sample_rate).abs() > 0.5 {
            self.rates = Rates::new(sample_rate);
        }
        self.kernel = kernel;
        self.panic();
        let patch = self.patch;
        self.set_patch(&patch);
    }

    /// Install a new patch; sounding notes follow the edit (Dexed refreshVoice).
    pub fn set_patch(&mut self, patch: &[u8; 156]) {
        self.patch = *patch;
        for v in self.voices.iter_mut() {
            if v.live {
                v.note.update(&self.patch, v.midi_note, v.velocity);
            }
        }
        self.lfo.reset(&self.rates, &self.patch[137..143]);
    }

    fn transpose(&self) -> i32 {
        i32::from(self.patch[144]) - 24
    }

    pub fn note_on(&mut self, pitch: u8, velo: u8) {
        if velo == 0 {
            self.note_off(pitch);
            return;
        }
        let key = i32::from(pitch);
        let pitch = (key + self.transpose()).clamp(0, 127);
        let velo = i32::from(velo);
        let nv = self.polyphony.clamp(1, MAX_VOICES);
        if self.current >= nv {
            self.current = 0;
        }

        // Steal preference: +4 not playing, +2 key up, +1 same pitch;
        // ties go to the oldest key press.
        let mut trigger_lfo = true;
        let mut best = self.current;
        let mut best_score = -1;
        let mut note = self.current;
        for _ in 0..nv {
            let v = &self.voices[note];
            if v.keydown {
                trigger_lfo = false;
            }
            let mut score = 0;
            if !v.note.is_playing() {
                score += 4;
            }
            if !v.keydown {
                score += 2;
            }
            if v.midi_note == pitch {
                score += 1;
            }
            if score > best_score || (score == best_score && v.keydown_seq < self.voices[best].keydown_seq) {
                best = note;
                best_score = score;
            }
            note = (note + 1) % nv;
        }
        if trigger_lfo {
            self.lfo.keydown();
        }
        let note = best;
        self.current = (note + 1) % nv;
        let seq = self.next_seq;
        self.next_seq += 1;
        let osc_sync = self.patch[136] != 0;
        {
            let v = &mut self.voices[note];
            v.key = key;
            v.midi_note = pitch;
            v.velocity = velo;
            v.sustained = self.sustain;
            v.keydown = true;
            v.keydown_seq = seq;
            let voice_steal = v.note.is_playing();
            v.note.hw_pitch = self.hw.hw_pitch;
            v.note.init(&self.rates, &self.patch, pitch, velo);
            v.note.pitch_offset = self.voice_pitch_offset[note];
            if osc_sync && !voice_steal {
                v.note.osc_sync();
            }
        }
        let last = self.last_active;
        if self.voices[last].midi_note != -1 && self.ctrls.porta_enabled() && last != note {
            let src = self.voices[last].note;
            self.voices[note].note.init_portamento(&src);
        }

        if self.mono {
            for i in 0..nv {
                if i == note || !self.voices[i].live {
                    continue;
                }
                if !self.voices[i].keydown {
                    // All keys were up: only carry the signal over.
                    self.voices[i].live = false;
                    let src = self.voices[i].note;
                    self.voices[note].note.transfer_signal(&src);
                    break;
                }
                if self.voices[i].midi_note > pitch {
                    // Mono priority: the highest note keeps sounding.
                    self.voices[i].live = false;
                    let src = self.voices[i].note;
                    self.voices[note].note.transfer_state(&src);
                    break;
                }
                // A higher-or-equal note is held: the new note stays silent.
                return;
            }
        } else if !osc_sync {
            // Same pitch already sounding: share its phase to avoid
            // unpredictable cancellation.
            for i in 0..nv {
                if i != note && self.voices[i].note.is_playing() && self.voices[i].midi_note == pitch {
                    let src = self.voices[i].note;
                    self.voices[note].note.transfer_phase(&src);
                    break;
                }
            }
        }
        self.voices[note].live = true;
        self.last_active = note;
    }

    pub fn note_off(&mut self, pitch: u8) {
        let key = i32::from(pitch);
        let Some(note) = self.voices.iter().position(|v| v.key == key && v.keydown) else {
            return;
        };
        self.voices[note].keydown = false;

        if self.mono {
            let mut high = -1;
            let mut target = 0;
            for (i, v) in self.voices.iter().enumerate() {
                if v.keydown && v.midi_note > high {
                    target = i;
                    high = v.midi_note;
                }
            }
            if high != -1 && self.voices[note].live {
                self.voices[note].live = false;
                self.voices[target].live = true;
                let src = self.voices[note].note;
                self.voices[target].note.transfer_state(&src);
            }
        }
        if self.sustain {
            self.voices[note].sustained = true;
        } else {
            self.voices[note].note.keyup();
        }
    }

    pub fn set_sustain(&mut self, on: bool) {
        self.sustain = on;
        if !on {
            for v in self.voices.iter_mut() {
                if v.sustained && !v.keydown {
                    v.note.keyup();
                    v.sustained = false;
                }
            }
        }
    }

    pub fn panic(&mut self) {
        for v in self.voices.iter_mut() {
            v.key = -1;
            v.midi_note = -1;
            v.keydown = false;
            v.live = false;
            v.sustained = false;
            v.note.osc_sync();
        }
    }

    /// Release voices beyond a lowered polyphony.
    pub fn set_polyphony(&mut self, n: usize) {
        let n = n.clamp(1, MAX_VOICES);
        if n < self.polyphony {
            for v in self.voices[n..].iter_mut() {
                v.live = false;
                v.keydown = false;
                v.midi_note = -1;
            }
        }
        self.polyphony = n;
    }

    pub fn set_mono(&mut self, mono: bool) {
        if mono != self.mono {
            self.mono = mono;
            self.panic();
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.live && v.note.is_playing()).count()
    }

    /// Render one block.
    pub fn render(&mut self, out: &mut Block) {
        out.mix = [0.0; N];
        out.vint = [0.0; N];
        let (lfo_val, lfo_delay) = if self.hw.lfo_steps {
            // The firmware updates LFO and modulation every ~132 samples
            // at 49.1 kHz (about two blocks): advance twice, hold twice.
            if self.lfo_tick & 1 == 0 {
                self.lfo.getsample();
                self.lfo.getdelay();
                self.lfo_hold = (self.lfo.getsample(), self.lfo.getdelay());
            }
            self.lfo_tick = self.lfo_tick.wrapping_add(1);
            self.lfo_hold
        } else {
            (self.lfo.getsample(), self.lfo.getdelay())
        };
        let hw = self.hw;
        for v in self.voices.iter_mut() {
            if !v.live {
                continue;
            }
            if !v.keydown && !v.sustained && !v.note.is_playing() && !self.mono {
                v.live = false;
                continue;
            }
            // The core adds carriers into this buffer ("has content" starts true).
            self.voice_buf = [0; N];
            v.note.compute(&self.rates, &mut self.core, self.kernel, &mut self.voice_buf, lfo_val, lfo_delay, &self.ctrls, hw.carrier_norm);
            for j in 0..N {
                let val = self.voice_buf[j];
                // Voice to 15-bit DAC word: one full carrier (2^25) = 16384.
                let v15 = (val >> 11).clamp(-16384, 16383);
                out.vint[j] += match hw.dac {
                    DacSw::Clean => val as f32 / 2048.0,
                    DacSw::Dx7 => {
                        let (q, r) = dx7_dac(v15);
                        q as f32 * hw.dac_gain[r]
                    }
                    DacSw::Flat12 => (v15 & !7) as f32,
                };
                // Dexed: >> 4, clip to 16 bits per voice, then float.
                let v4 = val >> 4;
                let clip = if v4 < -(1 << 24) { -0x8000 } else if v4 >= (1 << 24) { 0x7fff } else { v4 >> 9 };
                out.mix[j] += clip as f32 / 32768.0;
            }
        }
    }
}
