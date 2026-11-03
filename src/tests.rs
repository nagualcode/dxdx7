//! DSP, SysEx and plugin tests.

use crate::cartridges;
use crate::dx7::{Engine, Settings};
use crate::enums::EngineSw;
use crate::sysex::{self, Cartridge, Loaded};

const SR: f64 = 48000.0;

fn engine(kind: EngineSw) -> Engine {
    let mut e = Engine::new(SR);
    let s = Settings { engine: kind, noise: 0.0, ..Settings::default() };
    e.apply_settings(&s, true);
    e
}

fn render(e: &mut Engine, n: usize) -> Vec<f32> {
    (0..n).map(|_| e.process_sample()).collect()
}

/// Frequency from rising zero crossings with linear interpolation.
fn measure_hz(x: &[f32], sr: f64) -> f64 {
    let mut first = None;
    let mut last = 0.0;
    let mut count = 0;
    for i in 1..x.len() {
        // Debounce: the Mark I sine is sign-magnitude and can flicker
        // around zero by one LSB; ignore crossings closer than 20 samples.
        if x[i - 1] < 0.0 && x[i] >= 0.0 && (i as f64 - last > 20.0 || first.is_none()) {
            let t = (i - 1) as f64 + f64::from(-x[i - 1] / (x[i] - x[i - 1]));
            if first.is_none() {
                first = Some(t);
            } else {
                count += 1;
            }
            last = t;
        }
    }
    count as f64 * sr / (last - first.unwrap())
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

#[test]
fn init_voice_a4_is_440_on_every_engine() {
    for kind in [EngineSw::Modern, EngineSw::MarkI, EngineSw::Vintage] {
        let mut e = engine(kind);
        e.set_patch(&sysex::init_voice());
        e.note_on(69, 100);
        let x = render(&mut e, 48000);
        let hz = measure_hz(&x[4800..], SR);
        assert!(cents(hz, 440.0).abs() < 1.0, "{}: {hz} Hz", kind as u32);
    }
}

#[test]
fn sine_level_matches_between_engines() {
    let rms = |kind| {
        let mut e = engine(kind);
        e.set_patch(&sysex::init_voice());
        e.note_on(69, 100);
        let x = render(&mut e, 24000);
        (x[4800..].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / (x.len() - 4800) as f64).sqrt()
    };
    let m = rms(EngineSw::MarkI);
    let v = rms(EngineSw::Vintage);
    let d = 20.0 * (v / m).log10();
    assert!(d.abs() < 1.0, "vintage vs mark I level differs by {d:.2} dB");
}

#[test]
fn cartridge_round_trip() {
    for (i, (name, bytes)) in cartridges::BUILTIN.iter().enumerate() {
        let c = cartridges::builtin(i);
        let mut c2 = Cartridge::default();
        for k in 0..32 {
            c2.set_voice(k, &c.voice(k));
        }
        for k in 0..32 {
            assert_eq!(c.voice(k)[..155], c2.voice(k)[..155], "{name} voice {k}");
        }
        // The sysex we write loads back identically.
        let syx = c2.to_sysex();
        assert_eq!(syx.len(), sysex::CART_SYSEX_SIZE);
        match sysex::load(&syx).unwrap() {
            Loaded::Cart(c3, q) => {
                assert_eq!(q, sysex::LoadQuality::Ok);
                assert_eq!(c3.data, c2.data);
            }
            Loaded::Voice(_) => panic!("cart parsed as voice"),
        }
        assert!(bytes.len() >= 4104);
    }
}

#[test]
fn factory_roms_are_built_in() {
    assert_eq!(cartridges::BUILTIN[cartridges::ROM1A].0, "DX7_ROM1A");
    let c = cartridges::builtin(cartridges::ROM1A);
    // The famous ones: E.PIANO 1 is ROM1A #11, BASS 1 #15.
    assert_eq!(c.name(10).trim_end(), "E.PIANO 1");
    assert_eq!(c.name(14).trim_end(), "BASS    1");
    let c = cartridges::builtin(cartridges::ROM1A + 3); // ROM2B
    assert_eq!(c.name(13).trim_end(), "SYN-ORCH");
}

#[test]
fn single_voice_sysex_round_trip() {
    let c = cartridges::builtin(0);
    let v = c.voice(5);
    let syx = sysex::voice_to_sysex(&v);
    assert_eq!(syx.len(), 163);
    match sysex::load(&syx).unwrap() {
        Loaded::Voice(v2) => assert_eq!(v[..155], v2[..155]),
        Loaded::Cart(..) => panic!("voice parsed as cart"),
    }
}

#[test]
fn dexed_01_names() {
    let c = cartridges::builtin(0);
    assert_eq!(c.name(0), "Say Again.");
    assert_eq!(c.name(13), "-ANALOG 1-");
}

#[test]
fn every_builtin_voice_renders_finite_and_bounded() {
    for kind in [EngineSw::Modern, EngineSw::MarkI, EngineSw::Vintage] {
        let mut e = engine(kind);
        for cart in 0..cartridges::BUILTIN.len() {
            let c = cartridges::builtin(cart);
            for k in 0..32 {
                e.all_notes_off();
                e.set_patch(&c.voice(k));
                e.note_on(48, 100);
                e.note_on(60, 100);
                e.note_on(67, 127);
                let x = render(&mut e, 2400);
                let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
                assert!(x.iter().all(|v| v.is_finite()), "{} cart {cart} voice {k}: NaN", kind as u32);
                assert!(peak < 8.0, "{} cart {cart} voice {k}: peak {peak}", kind as u32);
            }
        }
    }
}

#[test]
fn dac_quantisation_follows_signal_level() {
    use crate::dx7::synth::dx7_dac;
    // Sign + 11-bit mantissa + 2-bit exponent: the step grows with level.
    assert_eq!(dx7_dac(1023), (1023, 3));
    assert_eq!(dx7_dac(1025), (1024, 2));
    assert_eq!(dx7_dac(2051), (2048, 1));
    assert_eq!(dx7_dac(16383), (16376, 0));
    assert_eq!(dx7_dac(-16383), (-16376, 0));
    for v in -16384..16384 {
        let (q, _) = dx7_dac(v);
        assert!((q - v).abs() < 8 && q.abs() <= v.abs());
        let rel = (q - v).abs() as f32 / v.abs().max(1) as f32;
        assert!(rel <= 2.0 / 1024.0, "{v} -> {q}");
    }
}

#[test]
fn hardware_pitch_grid() {
    use crate::dx7::note::{HW_STEP, osc_freq, osc_freq_hw};
    // A stays exact; the key pattern is 0 / -0.39 / -0.78 cents.
    let c = |a: i32, b: i32| f64::from(a - b) / f64::from(1 << 24) * 1200.0;
    assert_eq!(osc_freq_hw(69, 0, 1, 0, 7), osc_freq(69, 0, 1, 0, 7));
    let d1 = c(osc_freq_hw(70, 0, 1, 0, 7), osc_freq(70, 0, 1, 0, 7));
    let d2 = c(osc_freq_hw(71, 0, 1, 0, 7), osc_freq(71, 0, 1, 0, 7));
    assert!((d1 + 0.39).abs() < 0.05, "{d1}");
    assert!((d2 + 0.78).abs() < 0.05, "{d2}");
    // Every operator pitch is on the 1024/octave grid.
    let a4 = osc_freq(69, 0, 1, 0, 7);
    for coarse in 0..32 {
        for fine in [0, 7, 50, 99] {
            let f = osc_freq_hw(60, 0, coarse, fine, 9);
            assert_eq!((f - a4).rem_euclid(HW_STEP), 0);
        }
    }
    // x5 lands +0.41 cents sharp of a true 5:1 (firmware table rounding).
    let x5 = c(osc_freq_hw(69, 0, 5, 0, 7), osc_freq_hw(69, 0, 1, 0, 7)) - 1200.0 * 5f64.log2();
    assert!((x5 - 0.41).abs() < 0.6, "{x5}");
}

#[test]
fn carrier_scaling_lowers_six_carrier_algorithm() {
    // Algorithm 32 (six carriers): each carrier is scaled by about 1/6
    // (2688/1024 octaves = -15.8 dB) by the DX7's carrier compensation.
    let mut v = sysex::init_voice();
    v[134] = 31;
    // Only OP1 sounds, so nothing clips: the difference is pure scaling.
    v[5 * 21 + 16] = 99;
    let rms = |norm: bool| {
        let mut e = Engine::new(SR);
        let s = Settings { engine: EngineSw::Vintage, noise: 0.0, carrier_norm: norm, ..Settings::default() };
        e.apply_settings(&s, true);
        e.set_patch(&v);
        e.note_on(57, 100);
        let x = render(&mut e, 24000);
        (x[4800..].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / 19200.0).sqrt()
    };
    let d = 20.0 * (rms(true) / rms(false)).log10();
    assert!((d + 15.6).abs() < 1.5, "carrier scaling {d:.1} dB");
}

#[test]
fn mono_mode_keeps_one_voice() {
    let mut e = engine(EngineSw::MarkI);
    let s = Settings { engine: EngineSw::MarkI, mono: true, ..Settings::default() };
    e.apply_settings(&s, false);
    e.set_patch(&sysex::init_voice());
    e.note_on(60, 100);
    e.note_on(64, 100);
    e.note_on(67, 100);
    render(&mut e, 256);
    assert_eq!(e.active_voices(), 1);
}

#[test]
fn polyphony_limit() {
    let mut e = engine(EngineSw::MarkI);
    let s = Settings { engine: EngineSw::MarkI, polyphony: 4, ..Settings::default() };
    e.apply_settings(&s, false);
    e.set_patch(&sysex::init_voice());
    for n in 0..10 {
        e.note_on(50 + n, 100);
    }
    render(&mut e, 256);
    assert!(e.active_voices() <= 4);
}

// ---------------------------------------------------------------------------
// Plugin-level
// ---------------------------------------------------------------------------

#[test]
fn info_is_valid() {
    truce_test::assert_valid_info::<crate::Plugin>();
}

#[test]
fn has_editor() {
    truce_test::assert_has_editor::<crate::Plugin>();
}

#[test]
fn state_round_trips() {
    truce_test::assert_state_round_trip::<crate::Plugin>();
}

#[test]
fn params_read_back_as_voice() {
    use truce_params::Params;
    let p = crate::DxParams::new();
    let mut v = [0u8; 156];
    p.read_voice(&mut v);
    let init = sysex::init_voice();
    assert_eq!(v[..145], init[..145]);
    assert_eq!(v[155], 0x3F);
    // Every VCED byte maps to a distinct host parameter.
    let mut ids = crate::VOICE_PARAM_IDS.to_vec();
    ids.extend(crate::OP_ON_IDS);
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 145 + 6);
    assert!(p.param_infos().len() > 151);
}

#[test]
fn process_is_allocation_free() {
    use std::time::Duration;
    use truce_test::{assert_no_audio_alloc, driver};
    assert_no_audio_alloc(|| {
        driver!(crate::Plugin)
            .duration(Duration::from_millis(60))
            .script(|s| {
                s.note_on(60, 0.8);
                s.wait_ms(20);
                s.note_on(64, 0.8);
                s.wait_ms(20);
                s.note_off(60);
                s.note_off(64);
            })
            .run()
    });
}



#[test]
fn output_filter_matches_dx7_response() {
    // Analog prototype: +0.8 dB @ 15 kHz, -3 dB @ 17 kHz, -14 dB @ 20 kHz.
    // Measured through the whole Vintage path (DAC off-path: Clean) at 48
    // kHz relative to 1 kHz, the ZOH droop included (-1.4 dB @ 15 kHz).
    use crate::dx7::resample::Resampler;
    use crate::dx7::vintage::{DX7_SAMPLE_RATE, output_filter_response};
    for host in [44100.0, 48000.0, 96000.0] {
        let mut r = Resampler::new(DX7_SAMPLE_RATE, host, output_filter_response);
        let mut gain_db = |f: f64| {
            let mut n = 0u64;
            let mut acc = 0.0;
            let total = (host * 0.5) as usize;
            for i in 0..total {
                let y = r.next(|| {
                    n += 1;
                    (2.0 * std::f64::consts::PI * f * n as f64 / DX7_SAMPLE_RATE).sin() as f32
                });
                if i > total / 4 {
                    acc += f64::from(y) * f64::from(y);
                }
            }
            10.0 * (acc / (total - total / 4 - 1) as f64 / 0.5).log10()
        };
        let zoh = |f: f64| {
            let x = std::f64::consts::PI * f / DX7_SAMPLE_RATE;
            20.0 * (x.sin() / x).log10()
        };
        let mut checks = vec![(1000.0, 0.0), (10000.0, 20.0 * output_filter_response(10000.0).log10()), (15000.0, 0.8), (17000.0, -3.0)];
        if host > 44100.0 {
            checks.push((20000.0, -14.2));
        }
        for (f, want) in checks {
            let g = gain_db(f) - zoh(f);
            assert!((g - want).abs() < 0.5, "{host}: {f} Hz: {g:.2} dB, want {want}");
        }
    }
}
