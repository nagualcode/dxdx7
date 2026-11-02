//! Ableton-flavoured widgets bound to truce parameters.
//!
//! Continuous controls use the host gesture protocol (`begin_edit` /
//! `set_param` / `end_edit`) so automation recording captures a drag as one
//! gesture. Common affordances:
//! - drag vertically to change, Shift for fine control
//! - double-click to reset to the default
//! - mouse wheel for single steps

use std::collections::HashMap;
use std::sync::OnceLock;

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};
use truce_core::editor::{PluginContext, PluginContextReadF32};
use truce_params::Params;

use crate::DxParams;

pub mod theme {
    use egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
    pub const PANEL: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
    pub const PANEL_LINE: Color32 = Color32::from_rgb(0x36, 0x36, 0x36);
    pub const HEADER: Color32 = Color32::from_rgb(0x2f, 0x2f, 0x2f);
    pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xd8);
    pub const DIM: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
    pub const FAINT: Color32 = Color32::from_rgb(0x5a, 0x5a, 0x5a);
    pub const TRACK: Color32 = Color32::from_rgb(0x14, 0x14, 0x14);
    pub const CONTROL: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
    pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x48, 0x48, 0x48);
    /// Modulators / general accent (Ableton orange).
    pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x9a, 0x3c);
    /// Carriers (DX7 membrane-button teal).
    pub const CARRIER: Color32 = Color32::from_rgb(0x3c, 0xc8, 0xb4);
    pub const ON: Color32 = Color32::from_rgb(0xff, 0xb4, 0x4a);
    pub const ON_TEXT: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
    pub const DX_RED: Color32 = Color32::from_rgb(0xb8, 0x2a, 0x3a);
    pub const METER_LO: Color32 = Color32::from_rgb(0x5f, 0xd0, 0x6a);
    pub const METER_MID: Color32 = Color32::from_rgb(0xf0, 0xd2, 0x4a);
    pub const METER_HI: Color32 = Color32::from_rgb(0xff, 0x55, 0x45);
}
use theme::*;

pub type Ctx = PluginContext<DxParams>;

#[derive(Clone, Copy)]
struct Meta {
    default_norm: f32,
    steps: Option<u32>,
    bipolar: bool,
}

fn meta(state: &Ctx, id: u32) -> Meta {
    static CACHE: OnceLock<HashMap<u32, Meta>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        state
            .params()
            .param_infos()
            .iter()
            .map(|i| {
                let min = i.range.denormalize(0.0);
                (
                    i.id,
                    Meta {
                        default_norm: i.range.normalize(i.default_plain) as f32,
                        steps: i.range.step_count().map(|s| s.get()),
                        bipolar: min < 0.0,
                    },
                )
            })
            .collect()
    });
    map.get(&id).copied().unwrap_or(Meta { default_norm: 0.0, steps: None, bipolar: false })
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn snap(v: f32, steps: Option<u32>) -> f32 {
    match steps {
        Some(n) if n > 0 => (v * n as f32).round() / n as f32,
        _ => v,
    }
}

/// Shared drag / wheel / reset behaviour. `travel` = pixels for a full sweep.
fn drive(ui: &egui::Ui, state: &Ctx, resp: &egui::Response, id: u32, travel: f32) -> f32 {
    let m = meta(state, id);
    let mut value = state.get_param(id);
    let acc_id = resp.id.with("acc");
    if resp.double_clicked() {
        state.automate(id, f64::from(m.default_norm));
        return m.default_norm;
    }
    if resp.drag_started() {
        state.begin_edit(id);
        ui.memory_mut(|mem| mem.data.insert_temp(acc_id, value));
    }
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let scale = if fine { 0.15 } else { 1.0 };
        let delta = -resp.drag_delta().y / travel * scale;
        let raw: f32 = ui.memory(|mem| mem.data.get_temp(acc_id)).unwrap_or(value);
        let raw = (raw + delta).clamp(0.0, 1.0);
        ui.memory_mut(|mem| mem.data.insert_temp(acc_id, raw));
        let snapped = snap(raw, m.steps);
        if (snapped - value).abs() > 1e-6 {
            value = snapped;
            state.set_param(id, f64::from(value));
        }
    }
    if resp.drag_stopped() {
        state.end_edit(id);
    }
    if resp.hovered() && !resp.dragged() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let step = match m.steps {
                Some(n) if n > 0 => scroll.signum() / n as f32,
                _ => scroll / 1500.0,
            };
            value = (value + step).clamp(0.0, 1.0);
            state.automate(id, f64::from(value));
        }
    }
    value
}

/// Travel for a stepped parameter: ~3 px per step, at least 120 px.
fn travel_for(m: Meta) -> f32 {
    match m.steps {
        Some(n) => (n as f32 * 3.0).clamp(60.0, 300.0),
        None => 200.0,
    }
}

/// Compact numeric box (the Ableton "number field"): a value with a thin
/// fill bar showing its position. `text` overrides the displayed value.
pub fn value_box(ui: &mut egui::Ui, state: &Ctx, rect: Rect, id: impl Into<u32>, text: Option<&str>, accent: Color32) {
    let id = id.into();
    let m = meta(state, id);
    let resp = ui.interact(rect, ui.id().with(("vbox", id)), Sense::click_and_drag());
    let v = drive(ui, state, &resp, id, travel_for(m));
    let p = ui.painter();
    let active = resp.hovered() || resp.dragged();
    p.rect_filled(rect, CornerRadius::same(2), TRACK);
    // Position bar along the bottom edge.
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), pos2(rect.left() + rect.width() * v, rect.bottom()));
    p.rect_filled(bar, CornerRadius::ZERO, accent.gamma_multiply(if active { 1.0 } else { 0.55 }));
    if active {
        p.rect_stroke(rect, CornerRadius::same(2), Stroke::new(1.0_f32, accent), StrokeKind::Inside);
    }
    let s;
    let t = match text {
        Some(t) => t,
        None => {
            s = state.format_param(id);
            &s
        }
    };
    p.text(rect.center() - vec2(0.0, 1.0), Align2::CENTER_CENTER, t, mono(11.0), if active { Color32::WHITE } else { TEXT });
}

/// Labelled value box: label above, box below.
pub fn field(ui: &mut egui::Ui, state: &Ctx, cell: Rect, id: impl Into<u32>, label: &str) {
    ui.painter().text(pos2(cell.center().x, cell.top()), Align2::CENTER_TOP, label, font(9.5), DIM);
    let b = Rect::from_min_max(pos2(cell.left(), cell.top() + 13.0), pos2(cell.right(), cell.top() + 13.0 + 20.0));
    value_box(ui, state, b, id, None, ACCENT);
}

/// Ableton-style rotary dial with label above and value below.
pub fn knob(ui: &mut egui::Ui, state: &Ctx, cell: Rect, id: impl Into<u32>, label: &str) {
    let id = id.into();
    let m = meta(state, id);
    let p = ui.painter().clone();
    p.text(pos2(cell.center().x, cell.top()), Align2::CENTER_TOP, label, font(9.5), DIM);
    let r = 13.0;
    let c = pos2(cell.center().x, cell.top() + 14.0 + r + 2.0);
    let hit = Rect::from_center_size(c, Vec2::splat(2.0 * r + 8.0));
    let resp = ui.interact(hit, ui.id().with(("knob", id)), Sense::click_and_drag());
    let v = drive(ui, state, &resp, id, 160.0);
    const START: f32 = std::f32::consts::FRAC_PI_4 * 3.0;
    const SWEEP: f32 = std::f32::consts::FRAC_PI_2 * 3.0;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        (0..=32)
            .map(|i| {
                let a = START + SWEEP * (from + (to - from) * i as f32 / 32.0);
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    p.add(Shape::line(arc(0.0, 1.0), Stroke::new(3.0_f32, TRACK)));
    let base = if m.bipolar { 0.5 } else { 0.0 };
    if (v - base).abs() > 0.002 {
        p.add(Shape::line(arc(base.min(v), base.max(v)), Stroke::new(3.0_f32, ACCENT)));
    }
    let active = resp.hovered() || resp.dragged();
    let a = START + SWEEP * v;
    p.circle_filled(c, r - 5.0, if active { CONTROL_HOVER } else { CONTROL });
    p.line_segment([c + vec2(a.cos(), a.sin()) * 2.0, c + vec2(a.cos(), a.sin()) * (r - 4.0)], Stroke::new(2.0_f32, TEXT));
    p.text(pos2(cell.center().x, c.y + r + 3.0), Align2::CENTER_TOP, state.format_param(id), font(10.0), if active { ACCENT } else { TEXT });
}

pub fn button_face(p: &egui::Painter, rect: Rect, on: bool, hovered: bool, text: &str, size: f32, on_color: Color32) {
    let fill = if on {
        on_color
    } else if hovered {
        CONTROL_HOVER
    } else {
        CONTROL
    };
    p.rect_filled(rect, CornerRadius::same(2), fill);
    p.text(rect.center(), Align2::CENTER_CENTER, text, font(size), if on { ON_TEXT } else { TEXT });
}

/// Momentary push button (not bound to a parameter).
pub fn push(ui: &mut egui::Ui, rect: Rect, key: &str, text: &str) -> bool {
    let resp = ui.interact(rect, ui.id().with(("push", key)), Sense::click());
    button_face(ui.painter(), rect, false, resp.hovered(), text, 10.0, ON);
    resp.clicked()
}

/// Latching on/off button for a `BoolParam`.
pub fn toggle(ui: &mut egui::Ui, state: &Ctx, rect: Rect, id: impl Into<u32>, text: &str) {
    let id = id.into();
    let on = state.get_param(id) > 0.5;
    let resp = ui.interact(rect, ui.id().with(("toggle", id)), Sense::click());
    if resp.clicked() {
        state.automate(id, if on { 0.0 } else { 1.0 });
    }
    button_face(ui.painter(), rect, on, resp.hovered(), text, 9.5, ON);
}

/// Segmented selector for an enum / stepped parameter (index order).
pub fn selector(ui: &mut egui::Ui, state: &Ctx, rect: Rect, id: impl Into<u32>, labels: &[&str]) {
    let id = id.into();
    let n = labels.len();
    let cur = (state.get_param(id) * (n as f32 - 1.0)).round() as usize;
    let gap = 1.0;
    let w = (rect.width() - gap * (n as f32 - 1.0)) / n as f32;
    for (i, label) in labels.iter().enumerate() {
        let seg = Rect::from_min_size(pos2(rect.left() + i as f32 * (w + gap), rect.top()), vec2(w, rect.height()));
        let resp = ui.interact(seg, ui.id().with(("sel", id, i)), Sense::click());
        if resp.clicked() && i != cur {
            state.automate(id, i as f64 / (n as f64 - 1.0));
        }
        button_face(ui.painter(), seg, i == cur, resp.hovered(), label, 9.5, ON);
    }
}

/// Click-to-cycle box for a small enum (Shift-click goes back).
pub fn cycle_box(ui: &mut egui::Ui, state: &Ctx, rect: Rect, id: impl Into<u32>, labels: &[&str]) {
    let id = id.into();
    let n = labels.len();
    let cur = (state.get_param(id) * (n as f32 - 1.0)).round() as usize;
    let resp = ui.interact(rect, ui.id().with(("cyc", id)), Sense::click());
    if resp.clicked() {
        let back = ui.input(|i| i.modifiers.shift);
        let next = if back { (cur + n - 1) % n } else { (cur + 1) % n };
        state.automate(id, next as f64 / (n as f64 - 1.0));
    }
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(2), if resp.hovered() { CONTROL_HOVER } else { TRACK });
    p.text(rect.center(), Align2::CENTER_CENTER, labels[cur.min(n - 1)], mono(10.5), TEXT);
}

/// Section panel with a title bar; returns the content rect.
pub fn section(ui: &egui::Ui, rect: Rect, title: &str) -> Rect {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(3), PANEL);
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 18.0));
    p.rect_filled(head, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, HEADER);
    p.text(pos2(head.left() + 7.0, head.center().y), Align2::LEFT_CENTER, title, font(10.5), TEXT);
    Rect::from_min_max(pos2(rect.left() + 7.0, head.bottom() + 6.0), pos2(rect.right() - 7.0, rect.bottom() - 6.0))
}

/// Stereo peak meter (-48..+6 dBFS).
pub fn meter(ui: &egui::Ui, rect: Rect, l: f32, r: f32) {
    let p = ui.painter();
    let db_to_y = |db: f32| {
        let t = ((db + 48.0) / 54.0).clamp(0.0, 1.0);
        rect.bottom() - t * rect.height()
    };
    let bar_w = (rect.width() - 2.0) / 2.0;
    for (k, level) in [l, r].into_iter().enumerate() {
        let x0 = rect.left() + k as f32 * (bar_w + 2.0);
        let bar = Rect::from_min_size(pos2(x0, rect.top()), vec2(bar_w, rect.height()));
        p.rect_filled(bar, CornerRadius::ZERO, TRACK);
        let db = 20.0 * level.max(1e-6).log10();
        let y = db_to_y(db);
        for (lo, hi, col) in [(-48.0, -12.0, METER_LO), (-12.0, -3.0, METER_MID), (-3.0, 6.0, METER_HI)] {
            let top = db_to_y(hi).max(y);
            let bottom = db_to_y(lo);
            if top < bottom {
                p.rect_filled(Rect::from_min_max(pos2(x0, top), pos2(x0 + bar_w, bottom)), CornerRadius::ZERO, col);
            }
        }
    }
}
