//! Check .syx files with the plugin's own loader and list their voices.
//!
//! cargo run --release --example syx_info -- file1.syx [file2.syx ...]
//!
//! Prints the load result (cartridge / single voice, checksum state) and
//! the voice names, and renders each voice briefly to check it is finite.

use dxdx7::dx7::{Engine, Settings};
use dxdx7::sysex::{self, LoadQuality, Loaded};

fn main() {
    let mut fails = 0;
    let mut e = Engine::new(48000.0);
    e.apply_settings(&Settings::default(), true);
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                println!("{path}: read error {e}");
                fails += 1;
                continue;
            }
        };
        match sysex::load(&bytes) {
            Ok(Loaded::Cart(c, q)) => {
                let q = match q {
                    LoadQuality::Ok => "ok",
                    LoadQuality::BadChecksum => "bad checksum (loaded anyway)",
                    LoadQuality::Raw => "raw, no header",
                };
                let names: Vec<String> = (0..32).map(|i| c.name(i)).collect();
                let bad = (0..32).filter(|&i| !renders(&mut e, &c.voice(i))).count();
                println!("{path}: cartridge, {q}, {} bytes{}", bytes.len(), if bad > 0 { format!(", {bad} voices NOT finite") } else { String::new() });
                println!("  {}", names.join(" | "));
                fails += (bad > 0) as i32;
            }
            Ok(Loaded::Voice(v)) => println!("{path}: single voice '{}', renders: {}", sysex::voice_name(&v), renders(&mut e, &v)),
            Err(e) => {
                println!("{path}: NOT LOADED {e:?}");
                fails += 1;
            }
        }
    }
    std::process::exit(fails);
}

fn renders(e: &mut Engine, v: &sysex::Voice) -> bool {
    e.all_notes_off();
    e.set_patch(v);
    e.note_on(60, 100);
    let mut peak = 0f32;
    for i in 0..9600 {
        if i == 7200 {
            e.note_off(60);
        }
        let l = e.process_sample();
        if !l.is_finite() {
            return false;
        }
        peak = peak.max(l.abs());
    }
    peak < 8.0
}
