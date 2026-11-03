//! Editor: a flat, Ableton-style panel. Everything of a DX7 voice is on one
//! screen with no tabs:
//!
//! - header: cartridge and program browser, voice name, file actions,
//!   engine switch, output meter
//! - operator grid: one row per operator (OP1 at the top, like the DX7
//!   panel) with frequency, envelope, level, keyboard scaling and
//!   sensitivities; the row badge switches the operator on/off
//! - algorithm diagram with feedback, plus algorithm / feedback / key sync /
//!   transpose
//! - bottom strip: LFO, pitch EG, master, play/controllers and the Vintage
//!   hardware model.

mod widgets;

use std::sync::Arc;

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2};
use truce::prelude::*;
use truce_core::editor::PluginContextReadF32;
use truce_egui::{EditorUi, EguiEditor};

use crate::dx7::core::ALGORITHMS;
use crate::params::DxParamsParamId as P;
use crate::sysex::{self, Cartridge, Loaded, Voice};
use crate::{DxParams, OP_ON_IDS, VOICE_PARAM_IDS, VOICE_PARAM_MAX, cartridges, op_param_id, op_freq_label};
use widgets::theme::*;
use widgets::{Ctx, button_face, cycle_box, field, font, knob, meter, mono, push, section, selector, toggle, value_box};

const MARGIN: f32 = 8.0;
const GAP: f32 = 6.0;
const HEADER_H: f32 = 34.0;
const OPS_H: f32 = 256.0;
const BOTTOM_H: f32 = 132.0;
const ALG_W: f32 = 214.0;
const ROW_H: f32 = 34.0;
const BOX_H: f32 = 21.0;

pub const WIDTH: u32 = 1136;
pub const HEIGHT: u32 = (HEADER_H + MARGIN + OPS_H + GAP + BOTTOM_H + MARGIN) as u32;

pub fn editor(params: Arc<DxParams>) -> Box<dyn Editor> {
    Box::new(EguiEditor::with_ui(params, (WIDTH, HEIGHT), DxUi::default()).with_visuals(visuals()))
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = TRACK;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
    v.widgets.inactive.weak_bg_fill = CONTROL;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v
}

#[derive(Default)]
struct DxUi {
    name_buf: String,
    status: String,
    status_frames: u32,
}

// ---------------------------------------------------------------------------
// Voice <-> parameters
// ---------------------------------------------------------------------------

/// Push a whole voice to the host, one gesture per parameter.
fn apply_voice(ctx: &Ctx, v: &Voice) {
    for (b, (&id, &max)) in VOICE_PARAM_IDS.iter().zip(VOICE_PARAM_MAX.iter()).enumerate() {
        let norm = f64::from(v[b].min(max)) / f64::from(max);
        if (f64::from(ctx.get_param(id)) - norm).abs() > 1e-6 {
            ctx.automate(id, norm);
        }
    }
    for (k, &id) in OP_ON_IDS.iter().enumerate() {
        let on = v[155] & (1 << (5 - k)) != 0;
        ctx.automate(id, if on { 1.0 } else { 0.0 });
    }
}

fn current_voice(ctx: &Ctx) -> Voice {
    let mut v = sysex::init_voice();
    ctx.params().read_voice(&mut v);
    if let Ok(st) = ctx.params().patch.read() {
        sysex::set_voice_name(&mut v, &st.voice_name);
    }
    v
}

impl DxUi {
    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_frames = 240;
    }

    fn select_program(&mut self, ctx: &Ctx, prog: usize) {
        let v = {
            let Ok(mut st) = ctx.params().patch.write() else { return };
            let cart = st.cartridge();
            let v = cart.voice(prog);
            st.program = prog as u32;
            st.voice_name = sysex::voice_name(&v);
            self.name_buf = st.voice_name.trim_end().to_string();
            v
        };
        apply_voice(ctx, &v);
    }

    fn select_builtin(&mut self, ctx: &Ctx, idx: usize) {
        if let Ok(mut st) = ctx.params().patch.write() {
            st.cart = cartridges::builtin(idx).data.to_vec();
            st.cart_name = cartridges::BUILTIN[idx].0.to_string();
            st.builtin = idx as i32;
        }
        self.select_program(ctx, 0);
    }

    fn load_file(&mut self, ctx: &Ctx) {
        let Some(path) = rfd::FileDialog::new().add_filter("DX7 SysEx", &["syx", "SYX"]).add_filter("All files", &["*"]).pick_file() else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return self.set_status(format!("Can't read file: {e}")),
        };
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        match sysex::load(&bytes) {
            Ok(Loaded::Cart(c, q)) => {
                if let Ok(mut st) = ctx.params().patch.write() {
                    st.cart = c.data.to_vec();
                    st.cart_name = stem.clone();
                    st.builtin = -1;
                }
                self.select_program(ctx, 0);
                let note = match q {
                    sysex::LoadQuality::Ok => "",
                    sysex::LoadQuality::BadChecksum => " (checksum mismatch, loaded anyway)",
                    sysex::LoadQuality::Raw => " (no SysEx header, read as raw data)",
                };
                self.set_status(format!("Loaded cartridge {stem}{note}"));
            }
            Ok(Loaded::Voice(v)) => {
                if let Ok(mut st) = ctx.params().patch.write() {
                    st.voice_name = sysex::voice_name(&v);
                    self.name_buf = st.voice_name.trim_end().to_string();
                }
                apply_voice(ctx, &v);
                self.set_status(format!("Loaded voice {}", sysex::voice_name(&v).trim_end()));
            }
            Err(e) => self.set_status(format!("{stem}: {e}")),
        }
    }

    fn save_cart(&mut self, ctx: &Ctx) {
        let (cart, name) = match ctx.params().patch.read() {
            Ok(st) => (st.cartridge(), st.cart_name.clone()),
            Err(_) => return,
        };
        let Some(path) = rfd::FileDialog::new().add_filter("DX7 SysEx", &["syx"]).set_file_name(format!("{name}.syx")).save_file() else {
            return;
        };
        match std::fs::write(&path, cart.to_sysex()) {
            Ok(()) => self.set_status(format!("Saved {}", path.display())),
            Err(e) => self.set_status(format!("Save failed: {e}")),
        }
    }

    fn export_voice(&mut self, ctx: &Ctx) {
        let v = current_voice(ctx);
        let name = sysex::voice_name(&v).trim_end().replace(['/', '\\', ':'], "_");
        let Some(path) = rfd::FileDialog::new().add_filter("DX7 SysEx", &["syx"]).set_file_name(format!("{name}.syx")).save_file() else {
            return;
        };
        match std::fs::write(&path, sysex::voice_to_sysex(&v)) {
            Ok(()) => self.set_status(format!("Exported voice to {}", path.display())),
            Err(e) => self.set_status(format!("Export failed: {e}")),
        }
    }

    /// Write the edited voice into the current cartridge slot.
    fn store(&mut self, ctx: &Ctx) {
        let v = current_voice(ctx);
        if let Ok(mut st) = ctx.params().patch.write() {
            let mut c = st.cartridge();
            let prog = st.program as usize;
            c.set_voice(prog, &v);
            st.cart = c.data.to_vec();
            self.set_status(format!("Stored to slot {:02}", prog + 1));
        }
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

impl EditorUi<DxParams> for DxUi {
    fn opened(&mut self, ctx: &Ctx) {
        if let Ok(st) = ctx.params().patch.read() {
            self.name_buf = st.voice_name.trim_end().to_string();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, s: &Ctx) {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, CornerRadius::ZERO, BG);
        self.header(ui, s, full);

        let top = full.top() + HEADER_H + MARGIN;
        let ops_rect = Rect::from_min_max(pos2(full.left() + MARGIN, top), pos2(full.right() - MARGIN - ALG_W - GAP, top + OPS_H));
        let alg_rect = Rect::from_min_max(pos2(ops_rect.right() + GAP, top), pos2(full.right() - MARGIN, top + OPS_H));
        operators(ui, s, ops_rect);
        algorithm_section(ui, s, alg_rect);

        let by = top + OPS_H + GAP;
        let bottom = Rect::from_min_max(pos2(full.left() + MARGIN, by), pos2(full.right() - MARGIN, by + BOTTOM_H));
        bottom_strip(ui, s, bottom);

        if self.status_frames > 0 {
            self.status_frames -= 1;
        }
        ui.ctx().request_repaint();
    }
}

impl DxUi {
    fn header(&mut self, ui: &mut egui::Ui, s: &Ctx, full: Rect) {
        let p = ui.painter().clone();
        let bar = Rect::from_min_size(full.min, vec2(full.width(), HEADER_H));
        p.rect_filled(bar, CornerRadius::ZERO, PANEL);
        p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0_f32, PANEL_LINE));

        // Wordmark: DX7-style stripes + name.
        let x0 = bar.left() + MARGIN;
        for (k, c) in [DX_RED, CARRIER, ACCENT].into_iter().enumerate() {
            p.rect_filled(Rect::from_min_size(pos2(x0, bar.top() + 9.0 + k as f32 * 6.0), vec2(16.0, 3.0)), CornerRadius::same(1), c);
        }
        p.text(pos2(x0 + 22.0, bar.center().y), Align2::LEFT_CENTER, "DXDX7", font(17.0), TEXT);

        let (cart_name, builtin, program, cart, voice_name) = match s.params().patch.read() {
            Ok(st) => (st.cart_name.clone(), st.builtin, st.program as usize, st.cartridge(), st.voice_name.clone()),
            Err(_) => (String::new(), -1, 0, Cartridge::default(), String::new()),
        };
        let cy = bar.top() + 6.0;
        let h = 22.0;

        // Cartridge browser.
        let mut x = x0 + 92.0;
        let cart_r = Rect::from_min_size(pos2(x, cy), vec2(150.0, h));
        let mut pick_cart = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(cart_r), |ui| {
            egui::ComboBox::from_id_salt("cart")
                .width(cart_r.width() - 8.0)
                .height(520.0)
                .selected_text(egui::RichText::new(&cart_name).size(11.0))
                .show_ui(ui, |ui| {
                    for (i, (name, _)) in cartridges::BUILTIN.iter().enumerate() {
                        if ui.selectable_label(builtin == i as i32, *name).clicked() {
                            pick_cart = Some(i);
                        }
                    }
                });
        });
        if let Some(i) = pick_cart {
            self.select_builtin(s, i);
        }
        x += cart_r.width() + 4.0;

        // Program browser: prev / list / next.
        if push(ui, Rect::from_min_size(pos2(x, cy), vec2(20.0, h)), "prev", "◀") {
            self.select_program(s, (program + 31) % 32);
        }
        x += 22.0;
        let prog_r = Rect::from_min_size(pos2(x, cy), vec2(176.0, h));
        let mut pick_prog = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(prog_r), |ui| {
            egui::ComboBox::from_id_salt("prog")
                .width(prog_r.width() - 8.0)
                .height(620.0)
                .selected_text(egui::RichText::new(format!("{:02}  {}", program + 1, voice_name)).monospace().size(11.0))
                .show_ui(ui, |ui| {
                    for i in 0..32 {
                        let label = egui::RichText::new(format!("{:02}  {}", i + 1, cart.name(i))).monospace();
                        if ui.selectable_label(i == program, label).clicked() {
                            pick_prog = Some(i);
                        }
                    }
                });
        });
        if let Some(i) = pick_prog {
            self.select_program(s, i);
        }
        x += prog_r.width() + 2.0;
        if push(ui, Rect::from_min_size(pos2(x, cy), vec2(20.0, h)), "next", "▶") {
            self.select_program(s, (program + 1) % 32);
        }
        x += 30.0;

        // Voice name (10 characters, DX7 character set).
        p.text(pos2(x, bar.center().y), Align2::LEFT_CENTER, "NAME", font(9.5), DIM);
        x += 32.0;
        let name_r = Rect::from_min_size(pos2(x, cy), vec2(98.0, h));
        let name_id = ui.id().with("voice-name");
        if !ui.memory(|m| m.has_focus(name_id)) && self.name_buf != voice_name.trim_end() {
            self.name_buf = voice_name.trim_end().to_string();
        }
        let resp = ui.put(
            name_r,
            egui::TextEdit::singleline(&mut self.name_buf).id(name_id).char_limit(10).font(mono(11.5)).margin(vec2(4.0, 3.0)),
        );
        if resp.changed() {
            if let Ok(mut st) = s.params().patch.write() {
                st.voice_name = self.name_buf.clone();
            }
        }
        x += name_r.width() + 10.0;

        // File actions.
        let bw = 46.0;
        for (key, label) in [("load", "LOAD"), ("save", "SAVE"), ("store", "STORE"), ("export", "VOICE"), ("init", "INIT")] {
            let r = Rect::from_min_size(pos2(x, cy), vec2(bw, h));
            if push(ui, r, key, label) {
                match key {
                    "load" => self.load_file(s),
                    "save" => self.save_cart(s),
                    "store" => self.store(s),
                    "export" => self.export_voice(s),
                    _ => {
                        let v = sysex::init_voice();
                        if let Ok(mut st) = s.params().patch.write() {
                            st.voice_name = "INIT VOICE".into();
                        }
                        self.name_buf = "INIT VOICE".into();
                        apply_voice(s, &v);
                    }
                }
            }
            x += bw + 3.0;
        }

        // Meter on the right.
        let mx = bar.right() - MARGIN - 22.0;
        meter(ui, Rect::from_min_size(pos2(mx, bar.top() + 6.0), vec2(22.0, h)), s.get_meter(P::MeterL), s.get_meter(P::MeterR));
        // Engine switch (default DX7 VINTAGE).
        let eng = Rect::from_min_size(pos2(mx - 8.0 - 228.0, cy), vec2(228.0, h));
        selector(ui, s, eng, P::Engine, &["MODERN", "MARK I", "DX7 VINTAGE"]);

        // Status line (file results) between actions and engine switch.
        if self.status_frames > 0 && !self.status.is_empty() {
            let r = Rect::from_min_max(pos2(x + 6.0, cy), pos2(eng.left() - 8.0, cy + h));
            let galley = p.layout_no_wrap(self.status.clone(), font(10.0), ACCENT);
            let clip = p.with_clip_rect(r);
            clip.galley(pos2(r.left(), r.center().y - galley.size().y / 2.0), galley, ACCENT);
        }
    }
}

// ---------------------------------------------------------------------------
// Operator grid
// ---------------------------------------------------------------------------

/// Column layout of the operator grid: (key, header, width).
const COLS: &[(&str, &str, f32)] = &[
    ("op", "OP", 34.0),
    ("freq", "FREQ", 64.0),
    ("mode", "MODE", 34.0),
    ("crs", "CRS", 30.0),
    ("fine", "FINE", 30.0),
    ("det", "DET", 30.0),
    ("_", "", 6.0),
    ("eg", "ENVELOPE", 92.0),
    ("r1", "R1", 27.0),
    ("r2", "R2", 27.0),
    ("r3", "R3", 27.0),
    ("r4", "R4", 27.0),
    ("_", "", 3.0),
    ("l1", "L1", 27.0),
    ("l2", "L2", 27.0),
    ("l3", "L3", 27.0),
    ("l4", "L4", 27.0),
    ("_", "", 6.0),
    ("lvl", "LEVEL", 66.0),
    ("_", "", 6.0),
    ("bp", "BRK PT", 36.0),
    ("ld", "L DEP", 30.0),
    ("rd", "R DEP", 30.0),
    ("lc", "L CRV", 36.0),
    ("rc", "R CRV", 36.0),
    ("_", "", 6.0),
    ("rs", "RATE", 27.0),
    ("ams", "AMS", 27.0),
    ("kvs", "VEL", 27.0),
];

const CURVES: [&str; 4] = ["-LIN", "-EXP", "+EXP", "+LIN"];

fn operators(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "OPERATORS");
    let p = ui.painter().clone();
    let alg = (s.get_param_plain(P::Algorithm).round() as usize).min(31);

    // Column x positions (1 px gutter between cells).
    let mut xs = Vec::with_capacity(COLS.len());
    let mut x = c.left();
    for &(_, _, w) in COLS {
        xs.push((x, w));
        x += w + 1.0;
    }
    let cell = |key: &str, y: f32, h: f32| -> Rect {
        let i = COLS.iter().position(|c| c.0 == key).unwrap_or(0);
        let (x, w) = xs[i];
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    };
    for (i, &(key, label, _)) in COLS.iter().enumerate() {
        if key != "_" {
            let (x, w) = xs[i];
            p.text(pos2(x + w / 2.0, c.top()), Align2::CENTER_TOP, label, font(9.0), DIM);
        }
    }

    for n in 1..=6usize {
        let y = c.top() + 16.0 + (n - 1) as f32 * ROW_H;
        let slot = 6 - n;
        let carrier = ALGORITHMS[alg][slot] & 4 != 0;
        let tint = if carrier { CARRIER } else { ACCENT };
        let row = Rect::from_min_max(pos2(c.left() - 3.0, y - 3.0), pos2(c.right() + 3.0, y + BOX_H + 3.0));
        if n % 2 == 0 {
            p.rect_filled(row, CornerRadius::same(2), Color32::from_rgb(0x2a, 0x2a, 0x2a));
        }
        let id = |k: usize| op_param_id(n, k);
        let on = s.get_param(OP_ON_IDS[n - 1]) > 0.5;

        // Badge = on/off switch, coloured by role in the algorithm.
        let badge = cell("op", y, BOX_H);
        let resp = ui.interact(badge, ui.id().with(("opon", n)), Sense::click());
        if resp.clicked() {
            s.automate(OP_ON_IDS[n - 1], if on { 0.0 } else { 1.0 });
        }
        let fill = if on { tint } else if resp.hovered() { CONTROL_HOVER } else { CONTROL };
        p.rect_filled(badge, CornerRadius::same(2), fill);
        p.text(badge.center(), Align2::CENTER_CENTER, format!("{n}"), font(13.0), if on { ON_TEXT } else { DIM });

        // Frequency readout.
        let v = |k: usize| s.get_param_plain(id(k)).round() as u8;
        let fr = cell("freq", y, BOX_H);
        let fixed = v(17) == 1;
        let label = op_freq_label(v(17), v(18), v(19));
        let label = if fixed { label } else { format!("×{label}") };
        p.rect_filled(fr, CornerRadius::same(2), TRACK);
        p.text(fr.center(), Align2::CENTER_CENTER, label, mono(12.0), if on { tint } else { DIM });

        cycle_box(ui, s, cell("mode", y, BOX_H), id(17), &["RATIO", "FIXED"]);
        value_box(ui, s, cell("crs", y, BOX_H), id(18), None, tint);
        value_box(ui, s, cell("fine", y, BOX_H), id(19), None, tint);
        value_box(ui, s, cell("det", y, BOX_H), id(20), None, tint);

        let rates = [v(0), v(1), v(2), v(3)];
        let levels = [v(4), v(5), v(6), v(7)];
        env_graph(&p, cell("eg", y, BOX_H), rates, levels, if on { tint } else { FAINT }, 99.0);
        for (k, key) in ["r1", "r2", "r3", "r4", "l1", "l2", "l3", "l4"].iter().enumerate() {
            value_box(ui, s, cell(key, y, BOX_H), id(k), None, tint);
        }

        level_bar(ui, s, cell("lvl", y, BOX_H), id(16), tint);
        value_box(ui, s, cell("bp", y, BOX_H), id(8), None, tint);
        value_box(ui, s, cell("ld", y, BOX_H), id(9), None, tint);
        value_box(ui, s, cell("rd", y, BOX_H), id(10), None, tint);
        cycle_box(ui, s, cell("lc", y, BOX_H), id(11), &CURVES);
        cycle_box(ui, s, cell("rc", y, BOX_H), id(12), &CURVES);
        value_box(ui, s, cell("rs", y, BOX_H), id(13), None, tint);
        value_box(ui, s, cell("ams", y, BOX_H), id(14), None, tint);
        value_box(ui, s, cell("kvs", y, BOX_H), id(15), None, tint);
    }
    let foot = c.top() + 16.0 + 6.0 * ROW_H + 2.0;
    p.text(
        pos2(c.left(), foot),
        Align2::LEFT_TOP,
        "Drag to edit · Shift = fine · double-click = reset · click OP number = on/off · teal = carrier",
        font(9.5),
        FAINT,
    );
}

/// Horizontal output-level bar (0..99).
fn level_bar(ui: &mut egui::Ui, s: &Ctx, r: Rect, id: u32, tint: Color32) {
    value_box(ui, s, r, id, Some(""), tint);
    let v = s.get_param(id);
    let p = ui.painter();
    let inner = r.shrink2(vec2(3.0, 5.0));
    let fill = Rect::from_min_max(inner.min, pos2(inner.left() + inner.width() * v, inner.bottom()));
    p.rect_filled(fill, CornerRadius::same(1), tint.gamma_multiply(0.5));
    p.text(r.center(), Align2::CENTER_CENTER, s.format_param(id), mono(11.0), TEXT);
}

/// DX7 envelope sketch: starts at L4, R1->L1, R2->L2, R3->L3 (held while
/// the key is down), R4->L4 after release. Segment widths shrink with rate.
fn env_graph(p: &egui::Painter, r: Rect, rates: [u8; 4], levels: [u8; 4], col: Color32, max_level: f32) {
    p.rect_filled(r, CornerRadius::same(2), TRACK);
    let inner = r.shrink2(vec2(3.0, 3.0));
    let t = |rate: u8| {
        let x = 1.0 - f32::from(rate) / 99.0;
        0.04 + x * x * 1.6
    };
    let hold = 0.5;
    let ws = [t(rates[0]), t(rates[1]), t(rates[2]), hold, t(rates[3])];
    let total: f32 = ws.iter().sum();
    let sx = inner.width() / total;
    let y = |l: u8| inner.bottom() - f32::from(l) / max_level * inner.height();
    let mut x = inner.left();
    let mut pts = vec![pos2(x, y(levels[3]))];
    for (i, lv) in [levels[0], levels[1], levels[2], levels[2], levels[3]].into_iter().enumerate() {
        x += ws[i] * sx;
        pts.push(pos2(x, y(lv)));
    }
    let key_up = inner.left() + (ws[0] + ws[1] + ws[2] + ws[3]) * sx;
    p.line_segment([pos2(key_up, inner.top()), pos2(key_up, inner.bottom())], Stroke::new(1.0_f32, PANEL_LINE));
    let mut fill = pts.clone();
    fill.push(pos2(x, inner.bottom()));
    fill.insert(0, pos2(inner.left(), inner.bottom()));
    p.add(Shape::Path(egui::epaint::PathShape { points: fill, closed: true, fill: col.gamma_multiply(0.12), stroke: Stroke::NONE.into() }));
    p.add(Shape::line(pts, Stroke::new(1.3_f32, col)));
}

// ---------------------------------------------------------------------------
// Algorithm diagram
// ---------------------------------------------------------------------------

struct AlgGraph {
    /// edges[m][t]: operator m (1..6) modulates t.
    edges: [[bool; 7]; 7],
    carriers: Vec<usize>,
    /// Feedback (from, to).
    fb: Option<(usize, usize)>,
}

/// Recover the operator graph from the routing flags by replaying the
/// bus writes in render order (OP6 first).
fn alg_graph(alg: usize) -> AlgGraph {
    let flags = ALGORITHMS[alg & 31];
    let mut writers: [Vec<usize>; 3] = Default::default();
    let mut edges = [[false; 7]; 7];
    let mut carriers = Vec::new();
    let (mut fb_in, mut fb_out) = (None, Vec::new());
    for (slot, &f) in flags.iter().enumerate() {
        let op = 6 - slot;
        let inbus = usize::from((f >> 4) & 3);
        let outbus = usize::from(f & 3);
        let add = f & 4 != 0;
        if inbus != 0 {
            for &w in &writers[inbus] {
                edges[w][op] = true;
            }
        }
        if outbus == 0 {
            carriers.push(op);
        }
        if add {
            writers[outbus].push(op);
        } else {
            writers[outbus] = vec![op];
        }
        if f & 0x40 != 0 {
            fb_in = Some(op);
        }
        if f & 0x80 != 0 {
            fb_out.push(op);
        }
    }
    let fb = fb_in.map(|to| {
        let from = fb_out.iter().copied().find(|&o| o != to).unwrap_or(to);
        (from, to)
    });
    carriers.sort_unstable();
    AlgGraph { edges, carriers, fb }
}

fn algorithm_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "ALGORITHM");
    let alg = (s.get_param_plain(P::Algorithm).round() as usize).min(31);
    let g = alg_graph(alg);
    let p = ui.painter().clone();

    // Layout: carriers on the bottom row, each modulator above its
    // (first) target, spread across the target's column span.
    let mut pos: [Option<(f32, usize)>; 7] = [None; 7];
    let mut col = 0.0f32;
    fn place(op: usize, depth: usize, col: &mut f32, g: &AlgGraph, pos: &mut [Option<(f32, usize)>; 7]) -> (f32, f32) {
        let start = *col;
        let mods: Vec<usize> = (1..=6).filter(|&m| g.edges[m][op] && pos[m].is_none() && m != op).collect();
        for &m in &mods {
            pos[m] = Some((0.0, depth + 1)); // reserve
        }
        let mut spans = Vec::new();
        for &m in &mods {
            spans.push(place(m, depth + 1, col, g, pos));
        }
        if mods.is_empty() {
            *col += 1.0;
        }
        let end = *col;
        pos[op] = Some(((start + end) / 2.0, depth));
        (start, end)
    }
    for &cop in &g.carriers {
        if pos[cop].is_none() {
            place(cop, 0, &mut col, &g, &mut pos);
        }
    }
    let cols = col.max(1.0);
    let max_depth = pos.iter().flatten().map(|p| p.1).max().unwrap_or(0) as f32;

    let diag = Rect::from_min_max(c.min, pos2(c.right(), c.bottom() - 52.0));
    let bw = 24.0;
    let bh = 20.0;
    let step_x = ((diag.width() - 30.0) / cols).min(40.0);
    let step_y = ((diag.height() - bh - 14.0) / (max_depth.max(1.0))).min(36.0);
    let origin_x = diag.center().x - step_x * cols / 2.0;
    let base_y = diag.bottom() - 14.0 - bh / 2.0;
    let center = |op: usize| -> Pos2 {
        let (cx, d) = pos[op].unwrap_or((0.0, 0));
        pos2(origin_x + cx * step_x, base_y - d as f32 * step_y)
    };
    let line = Stroke::new(1.2_f32, DIM);
    for m in 1..=6 {
        for t in 1..=6 {
            if g.edges[m][t] {
                p.line_segment([center(m) + vec2(0.0, bh / 2.0), center(t) - vec2(0.0, bh / 2.0)], line);
            }
        }
    }
    // Output bus under the carriers.
    if let (Some(&first), Some(&last)) = (g.carriers.iter().min_by(|a, b| center(**a).x.total_cmp(&center(**b).x)), g.carriers.iter().max_by(|a, b| center(**a).x.total_cmp(&center(**b).x))) {
        let yb = base_y + bh / 2.0 + 7.0;
        p.line_segment([pos2(center(first).x, yb), pos2(center(last).x, yb)], Stroke::new(1.5_f32, CARRIER));
        for &cop in &g.carriers {
            p.line_segment([center(cop) + vec2(0.0, bh / 2.0), pos2(center(cop).x, yb)], Stroke::new(1.5_f32, CARRIER));
        }
    }
    // Feedback loop: from the source's right edge up and over to the target.
    if let Some((from, to)) = g.fb {
        let fbv = s.get_param_plain(P::Feedback).round();
        let col = if fbv > 0.0 { ACCENT } else { FAINT };
        let a = center(from) + vec2(bw / 2.0, 0.0);
        let b = center(to) + vec2(bw / 2.0, 0.0);
        let xr = a.x.max(b.x) + 8.0;
        let top = center(to).y - bh / 2.0 - 6.0;
        let pts = vec![a, pos2(xr, a.y), pos2(xr, top), pos2(center(to).x, top), pos2(center(to).x, center(to).y - bh / 2.0)];
        let _ = b;
        p.add(Shape::line(pts, Stroke::new(1.2_f32, col)));
    }
    for op in 1..=6 {
        let ctr = center(op);
        let br = Rect::from_center_size(ctr, vec2(bw, bh));
        let on = s.get_param(OP_ON_IDS[op - 1]) > 0.5;
        let carrier = g.carriers.contains(&op);
        let tint = if carrier { CARRIER } else { ACCENT };
        p.rect_filled(br, CornerRadius::same(2), if on { tint } else { CONTROL });
        p.rect_stroke(br, CornerRadius::same(2), Stroke::new(1.0_f32, if on { tint } else { FAINT }), StrokeKind::Inside);
        p.text(ctr, Align2::CENTER_CENTER, format!("{op}"), font(11.5), if on { ON_TEXT } else { DIM });
    }

    // Controls under the diagram.
    let y = c.bottom() - 34.0;
    let w = (c.width() - 9.0) / 4.0;
    let cellr = |i: usize| Rect::from_min_size(pos2(c.left() + i as f32 * (w + 3.0), y), vec2(w, 34.0));
    field(ui, s, cellr(0), P::Algorithm, "ALG");
    field(ui, s, cellr(1), P::Feedback, "FEEDBK");
    let ks = cellr(2);
    p.text(pos2(ks.center().x, ks.top()), Align2::CENTER_TOP, "KEY SYNC", font(9.5), DIM);
    toggle(ui, s, Rect::from_min_size(pos2(ks.left(), ks.top() + 13.0), vec2(ks.width(), 20.0)), P::OscSync, "OSC");
    field(ui, s, cellr(3), P::Transpose, "TRANSP");
}

// ---------------------------------------------------------------------------
// Bottom strip
// ---------------------------------------------------------------------------

fn bottom_strip(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let widths = [232.0, 236.0, 196.0, 166.0, 0.0];
    let mut x = r.left();
    let mut secs = Vec::new();
    for (i, &w) in widths.iter().enumerate() {
        let w = if i == widths.len() - 1 { r.right() - x } else { w };
        secs.push(Rect::from_min_size(pos2(x, r.top()), vec2(w, r.height())));
        x += w + GAP;
    }
    lfo_section(ui, s, secs[0]);
    pitch_eg_section(ui, s, secs[1]);
    master_section(ui, s, secs[2]);
    play_section(ui, s, secs[3]);
    vintage_section(ui, s, secs[4]);
}

/// Grid cell `i` of `n` columns in row `row` (row height 36).
fn grid(c: Rect, n: usize, i: usize, row: usize) -> Rect {
    let w = (c.width() - (n as f32 - 1.0) * 3.0) / n as f32;
    Rect::from_min_size(pos2(c.left() + i as f32 * (w + 3.0), c.top() + row as f32 * 38.0), vec2(w, 34.0))
}

fn lfo_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "LFO");
    field(ui, s, grid(c, 4, 0, 0), P::LfoSpeed, "SPEED");
    field(ui, s, grid(c, 4, 1, 0), P::LfoDelay, "DELAY");
    field(ui, s, grid(c, 4, 2, 0), P::LfoPmd, "PMD");
    field(ui, s, grid(c, 4, 3, 0), P::LfoAmd, "AMD");
    field(ui, s, grid(c, 4, 0, 1), P::Pms, "PMS");
    let w = grid(c, 4, 1, 1).union(grid(c, 4, 2, 1));
    ui.painter().text(pos2(w.center().x, w.top()), Align2::CENTER_TOP, "WAVE", font(9.5), DIM);
    cycle_box(ui, s, Rect::from_min_size(pos2(w.left(), w.top() + 13.0), vec2(w.width(), 20.0)), P::LfoWave, &["TRIANGLE", "SAW DOWN", "SAW UP", "SQUARE", "SINE", "S&HOLD"]);
    let k = grid(c, 4, 3, 1);
    ui.painter().text(pos2(k.center().x, k.top()), Align2::CENTER_TOP, "KEY SYNC", font(9.5), DIM);
    toggle(ui, s, Rect::from_min_size(pos2(k.left(), k.top() + 13.0), vec2(k.width(), 20.0)), P::LfoSync, "SYNC");
}

fn pitch_eg_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "PITCH EG");
    let ids_r = [P::PegR1, P::PegR2, P::PegR3, P::PegR4];
    let ids_l = [P::PegL1, P::PegL2, P::PegL3, P::PegL4];
    for i in 0..4 {
        field(ui, s, grid(c, 4, i, 0), ids_r[i], ["R1", "R2", "R3", "R4"][i]);
        let cell = grid(c, 4, i, 1);
        ui.painter().text(pos2(cell.center().x, cell.top()), Align2::CENTER_TOP, ["L1", "L2", "L3", "L4"][i], font(9.5), DIM);
        let b = Rect::from_min_size(pos2(cell.left(), cell.top() + 13.0), vec2(cell.width(), 20.0));
        let v = s.get_param_plain(ids_l[i]).round() as i32;
        value_box(ui, s, b, ids_l[i], Some(&format!("{v}")), ACCENT);
    }
    let g = Rect::from_min_max(pos2(c.left(), c.top() + 2.0 * 38.0), pos2(c.right(), c.bottom()));
    let rates = ids_r.map(|id| s.get_param_plain(id).round() as u8);
    let levels = ids_l.map(|id| s.get_param_plain(id).round() as u8);
    env_graph(ui.painter(), g, rates, levels, ACCENT, 99.0);
    // Centre line (level 50 = no pitch change).
    let inner = g.shrink2(vec2(3.0, 3.0));
    let yc = inner.bottom() - 50.0 / 99.0 * inner.height();
    ui.painter().line_segment([pos2(inner.left(), yc), pos2(inner.right(), yc)], Stroke::new(1.0_f32, PANEL_LINE));
}

fn master_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "MASTER");
    let w = c.width() / 4.0;
    let cell = |i: usize| Rect::from_min_size(pos2(c.left() + i as f32 * w, c.top()), vec2(w, 60.0));
    knob(ui, s, cell(0), P::Output, "OUTPUT");
    knob(ui, s, cell(1), P::Cutoff, "CUTOFF");
    knob(ui, s, cell(2), P::Resonance, "RESO");
    knob(ui, s, cell(3), P::MasterTune, "TUNE");
    let row = Rect::from_min_max(pos2(c.left(), c.top() + 66.0), pos2(c.right(), c.bottom()));
    field(ui, s, grid(row, 4, 0, 0), P::Polyphony, "VOICES");
    let m = grid(row, 4, 1, 0);
    ui.painter().text(pos2(m.center().x, m.top()), Align2::CENTER_TOP, "MODE", font(9.5), DIM);
    toggle(ui, s, Rect::from_min_size(pos2(m.left(), m.top() + 13.0), vec2(m.width(), 20.0)), P::Mono, "MONO");
    let v = grid(row, 4, 2, 0).union(grid(row, 4, 3, 0));
    ui.painter().text(pos2(v.center().x, v.top()), Align2::CENTER_TOP, "VELOCITY", font(9.5), DIM);
    selector(ui, s, Rect::from_min_size(pos2(v.left(), v.top() + 13.0), vec2(v.width(), 20.0)), P::VelCurve, &["MIDI", "DX7"]);
}

fn play_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let c = section(ui, r, "CONTROLLERS");
    // Pitch bend + portamento row.
    field(ui, s, grid(c, 5, 0, 0), P::BendUp, "PB+");
    field(ui, s, grid(c, 5, 1, 0), P::BendDown, "PB−");
    field(ui, s, grid(c, 5, 2, 0), P::BendStep, "STEP");
    field(ui, s, grid(c, 5, 3, 0), P::PortaTime, "PORTA");
    let g = grid(c, 5, 4, 0);
    ui.painter().text(pos2(g.center().x, g.top()), Align2::CENTER_TOP, "GLIS", font(9.5), DIM);
    toggle(ui, s, Rect::from_min_size(pos2(g.left(), g.top() + 13.0), vec2(g.width(), 20.0)), P::Glissando, "G");
    // Mod sources: range + pitch / amp / EG bias.
    let rows = [
        ("WHL", P::WheelRange, P::WheelPitch, P::WheelAmp, P::WheelEg),
        ("FOOT", P::FootRange, P::FootPitch, P::FootAmp, P::FootEg),
        ("BRTH", P::BreathRange, P::BreathPitch, P::BreathAmp, P::BreathEg),
        ("AT", P::AtRange, P::AtPitch, P::AtAmp, P::AtEg),
    ];
    let top = c.top() + 40.0;
    let rh = ((c.bottom() - top) / 4.0).min(15.0);
    let p = ui.painter().clone();
    for (i, (name, range, pt, am, eg)) in rows.into_iter().enumerate() {
        let y = top + i as f32 * (rh + 1.0);
        p.text(pos2(c.left(), y + rh / 2.0), Align2::LEFT_CENTER, name, font(9.0), DIM);
        let x0 = c.left() + 32.0;
        value_box(ui, s, Rect::from_min_size(pos2(x0, y), vec2(34.0, rh)), range, None, ACCENT);
        for (k, (id, l)) in [(pt, "P"), (am, "A"), (eg, "E")].into_iter().enumerate() {
            toggle(ui, s, Rect::from_min_size(pos2(x0 + 38.0 + k as f32 * 28.0, y), vec2(26.0, rh)), id, l);
        }
    }
}

fn vintage_section(ui: &mut egui::Ui, s: &Ctx, r: Rect) {
    let vintage = s.get_param(P::Engine) > 0.75;
    let c = section(ui, r, if vintage { "DX7 HARDWARE MODEL" } else { "DX7 HARDWARE MODEL  (select DX7 VINTAGE)" });
    let p = ui.painter().clone();
    // DAC (default DX7 Mk I).
    let d = grid(c, 4, 0, 0).union(grid(c, 4, 1, 0));
    p.text(pos2(d.center().x, d.top()), Align2::CENTER_TOP, "DAC", font(9.5), DIM);
    selector(ui, s, Rect::from_min_size(pos2(d.left(), d.top() + 13.0), vec2(d.width(), 20.0)), P::Dac, &["CLEAN", "MK I", "12-BIT"]);
    let t = grid(c, 4, 2, 0).union(grid(c, 4, 3, 0));
    p.text(pos2(t.center().x, t.top()), Align2::CENTER_TOP, "CIRCUIT", font(9.5), DIM);
    let tw = (t.width() - 9.0) / 4.0;
    for (k, (id, l)) in [(P::OutFilter, "FILTER"), (P::HwPitch, "PITCH"), (P::CarrierNorm, "CARR"), (P::LfoSteps, "LFO")].into_iter().enumerate() {
        toggle(ui, s, Rect::from_min_size(pos2(t.left() + k as f32 * (tw + 3.0), t.top() + 13.0), vec2(tw, 20.0)), id, l);
    }
    let w = c.width() / 3.0;
    for (k, (id, l)) in [(P::AnalogNoise, "HISS"), (P::Drive, "DRIVE"), (P::Tolerance, "TOLERANCE")].into_iter().enumerate() {
        knob(ui, s, Rect::from_min_size(pos2(c.left() + k as f32 * w, c.top() + 38.0), vec2(w, 60.0)), id, l);
    }
    if !vintage {
        ui.painter().rect_filled(c.expand(4.0), CornerRadius::same(2), BG.gamma_multiply(0.55));
    }
    let _ = button_face;
}
