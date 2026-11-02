//! Offline render of a voice to WAV, for listening tests and spectra.
//!
//! cargo run --release --example render -- [cart_idx] [voice_idx] [engine 0|1|2] [note] [out.wav] [secs]

use dxdx7::cartridges;
use dxdx7::dx7::{Engine, Settings};
use dxdx7::enums::EngineSw;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cart = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(0usize);
    let voice = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0usize);
    let engine = match a.get(3).map(String::as_str) {
        Some("0") => EngineSw::Modern,
        Some("1") => EngineSw::MarkI,
        _ => EngineSw::Vintage,
    };
    let note: u8 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
    let out = a.get(5).cloned().unwrap_or_else(|| "render.wav".into());
    let secs: f64 = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let sr = 48000.0;
    let mut e = Engine::new(sr);
    let s = Settings { engine, ..Settings::default() };
    e.apply_settings(&s, true);
    let c = cartridges::builtin(cart);
    let v = c.voice(voice);
    eprintln!("{} / {}", cartridges::BUILTIN[cart].0, c.name(voice));
    e.set_patch(&v);
    let spec = hound::WavSpec { channels: 1, sample_rate: sr as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&out, spec).unwrap();
    let total = (secs * sr) as usize;
    let off = (total as f64 * 0.7) as usize;
    let mut peak = 0f32;
    let mut sum2 = 0f64;
    for i in 0..total {
        if i == 0 {
            e.note_on(note, 100);
        }
        if i == off {
            e.note_off(note);
        }
        let y = e.process_sample();
        peak = peak.max(y.abs());
        sum2 += f64::from(y) * f64::from(y);
        w.write_sample(y).unwrap();
    }
    w.finalize().unwrap();
    eprintln!("peak {:.3} ({:.1} dBFS)  rms {:.1} dBFS", peak, 20.0 * peak.log10(), 10.0 * (sum2 / total as f64).log10());
}
