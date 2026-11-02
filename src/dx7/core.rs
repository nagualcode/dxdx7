//! The FM operator core: six operators wired by one of the 32 DX7
//! algorithms. Two kernels:
//! - `Modern`: msfa's high-resolution sine with linear gain.
//! - `MarkI`: Dexed's EngineMkI, which works like the OPS chip: a log-sine
//!   lookup, the envelope added in the log domain, then an exp lookup.

use super::tables::{LG_N, N, tables};

#[derive(Clone, Copy, Default)]
pub struct OpParams {
    /// Envelope + modulation level, Q24 log2.
    pub level_in: i32,
    /// Gain reached at the end of the previous block (for ramping).
    pub gain_out: i32,
    /// Q24 phase increment per sample.
    pub freq: i32,
    pub phase: i32,
}

const OUT_BUS_ADD: u8 = 1 << 2;
const FB_IN: u8 = 1 << 6;
const FB_OUT: u8 = 1 << 7;

/// Per-operator routing flags (operator 0 = OP6): bits 0-1 output bus
/// (0 = voice output), bit 2 add to bus, bits 4-5 input bus, bits 6-7
/// feedback in/out.
pub const ALGORITHMS: [[u8; 6]; 32] = [
    [0xc1, 0x11, 0x11, 0x14, 0x01, 0x14], // 1
    [0x01, 0x11, 0x11, 0x14, 0xc1, 0x14], // 2
    [0xc1, 0x11, 0x14, 0x01, 0x11, 0x14], // 3
    [0xc1, 0x11, 0x94, 0x01, 0x11, 0x14], // 4
    [0xc1, 0x14, 0x01, 0x14, 0x01, 0x14], // 5
    [0xc1, 0x94, 0x01, 0x14, 0x01, 0x14], // 6
    [0xc1, 0x11, 0x05, 0x14, 0x01, 0x14], // 7
    [0x01, 0x11, 0xc5, 0x14, 0x01, 0x14], // 8
    [0x01, 0x11, 0x05, 0x14, 0xc1, 0x14], // 9
    [0x01, 0x05, 0x14, 0xc1, 0x11, 0x14], // 10
    [0xc1, 0x05, 0x14, 0x01, 0x11, 0x14], // 11
    [0x01, 0x05, 0x05, 0x14, 0xc1, 0x14], // 12
    [0xc1, 0x05, 0x05, 0x14, 0x01, 0x14], // 13
    [0xc1, 0x05, 0x11, 0x14, 0x01, 0x14], // 14
    [0x01, 0x05, 0x11, 0x14, 0xc1, 0x14], // 15
    [0xc1, 0x11, 0x02, 0x25, 0x05, 0x14], // 16
    [0x01, 0x11, 0x02, 0x25, 0xc5, 0x14], // 17
    [0x01, 0x11, 0x11, 0xc5, 0x05, 0x14], // 18
    [0xc1, 0x14, 0x14, 0x01, 0x11, 0x14], // 19
    [0x01, 0x05, 0x14, 0xc1, 0x14, 0x14], // 20
    [0x01, 0x14, 0x14, 0xc1, 0x14, 0x14], // 21
    [0xc1, 0x14, 0x14, 0x14, 0x01, 0x14], // 22
    [0xc1, 0x14, 0x14, 0x01, 0x14, 0x04], // 23
    [0xc1, 0x14, 0x14, 0x14, 0x04, 0x04], // 24
    [0xc1, 0x14, 0x14, 0x04, 0x04, 0x04], // 25
    [0xc1, 0x05, 0x14, 0x01, 0x14, 0x04], // 26
    [0x01, 0x05, 0x14, 0xc1, 0x14, 0x04], // 27
    [0x04, 0xc1, 0x11, 0x14, 0x01, 0x14], // 28
    [0xc1, 0x14, 0x01, 0x14, 0x04, 0x04], // 29
    [0x04, 0xc1, 0x11, 0x14, 0x04, 0x04], // 30
    [0xc1, 0x14, 0x04, 0x04, 0x04, 0x04], // 31
    [0xc4, 0x04, 0x04, 0x04, 0x04, 0x04], // 32
];

pub fn is_carrier(algorithm: usize, op: usize) -> bool {
    ALGORITHMS[algorithm & 31][op] & OUT_BUS_ADD != 0
}

/// DX7 carrier-count compensation ("COM", OPS algorithm ROM / US 4,554,857):
/// every carrier is attenuated by log2(carriers) in the log domain, in
/// units of 1024 per 6 dB, before the exp lookup. Dexed omits it.
const COM: [i32; 7] = [0, 0, 1024, 1664, 2048, 2432, 2688];

pub fn carrier_count(algorithm: usize) -> usize {
    (0..6).filter(|&op| is_carrier(algorithm, op)).count()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    Modern,
    MarkI,
}

/// Scratch buses shared by all voices (one voice renders at a time).
pub struct Core {
    bus: [[i32; N]; 2],
}

impl Default for Core {
    fn default() -> Self {
        Self { bus: [[0; N]; 2] }
    }
}

const ENV_BITDEPTH: i32 = 14;
const ENV_MAX: i32 = 1 << ENV_BITDEPTH;

#[inline(always)]
fn dgain(g1: i32, g2: i32) -> i32 {
    (g2 - g1 + ((N as i32) >> 1)) >> LG_N
}

impl Core {
    /// Render one block of one voice into `out` (overwritten).
    /// `carrier_norm`: apply the DX7's carrier-count compensation (Mark I kernel).
    pub fn render(&mut self, kernel: Kernel, out: &mut [i32; N], params: &mut [OpParams; 6], algorithm: usize, fb_buf: &mut [i32; 2], fb_shift: i32, carrier_norm: bool) {
        match kernel {
            Kernel::Modern => self.render_modern(out, params, algorithm, fb_buf, fb_shift),
            Kernel::MarkI => self.render_mki(out, params, algorithm, fb_buf, fb_shift, carrier_norm),
        }
    }

    fn bus_ptr<'a>(out: &'a mut [i32; N], bus: &'a mut [[i32; N]; 2], outbus: usize, inbus: usize) -> (&'a mut [i32; N], Option<&'a [i32; N]>) {
        // Borrow the output bus mutably and the input bus immutably.
        let [b1, b2] = bus;
        match (outbus, inbus) {
            (0, 0) => (out, None),
            (0, 1) => (out, Some(b1)),
            (0, 2) => (out, Some(b2)),
            (1, 0) => (b1, None),
            (1, 1) => (b1, None), // same bus in and out never happens in DX7 tables
            (1, 2) => (b1, Some(b2)),
            (2, 0) => (b2, None),
            (2, 1) => (b2, Some(b1)),
            _ => (b2, None),
        }
    }

    fn render_modern(&mut self, out: &mut [i32; N], params: &mut [OpParams; 6], algorithm: usize, fb_buf: &mut [i32; 2], fb_shift: i32) {
        const LEVEL_THRESH: i32 = 1120;
        let t = tables();
        let alg = ALGORITHMS[algorithm & 31];
        let mut has = [true, false, false];
        for op in 0..6 {
            let flags = alg[op];
            let mut add = flags & OUT_BUS_ADD != 0;
            let inbus = usize::from((flags >> 4) & 3);
            let outbus = usize::from(flags & 3);
            let p = &mut params[op];
            let gain1 = p.gain_out;
            let gain2 = t.exp2(p.level_in - (14 << 24));
            p.gain_out = gain2;
            if gain1 >= LEVEL_THRESH || gain2 >= LEVEL_THRESH {
                if !has[outbus] {
                    add = false;
                }
                let input_live = inbus != 0 && has[inbus];
                let (o, input) = Self::bus_ptr(out, &mut self.bus, outbus, if input_live { inbus } else { 0 });
                let dg = dgain(gain1, gain2);
                let mut gain = gain1;
                let mut phase = p.phase;
                let freq = p.freq;
                if let Some(inp) = input {
                    for i in 0..N {
                        gain += dg;
                        let y = t.sin(phase.wrapping_add(inp[i]));
                        let y1 = ((i64::from(y) * i64::from(gain)) >> 24) as i32;
                        o[i] = if add { o[i].wrapping_add(y1) } else { y1 };
                        phase = phase.wrapping_add(freq);
                    }
                } else if (flags & (FB_IN | FB_OUT)) == (FB_IN | FB_OUT) && fb_shift < 16 {
                    let mut y0 = fb_buf[0];
                    let mut y = fb_buf[1];
                    for i in 0..N {
                        gain += dg;
                        let scaled_fb = (y0.wrapping_add(y)) >> (fb_shift + 1);
                        y0 = y;
                        y = t.sin(phase.wrapping_add(scaled_fb));
                        y = ((i64::from(y) * i64::from(gain)) >> 24) as i32;
                        o[i] = if add { o[i].wrapping_add(y) } else { y };
                        phase = phase.wrapping_add(freq);
                    }
                    fb_buf[0] = y0;
                    fb_buf[1] = y;
                } else {
                    for i in 0..N {
                        gain += dg;
                        let y = t.sin(phase);
                        let y1 = ((i64::from(y) * i64::from(gain)) >> 24) as i32;
                        o[i] = if add { o[i].wrapping_add(y1) } else { y1 };
                        phase = phase.wrapping_add(freq);
                    }
                }
                has[outbus] = true;
            } else if !add {
                has[outbus] = false;
            }
            p.phase = p.phase.wrapping_add(p.freq << LG_N);
        }
    }

    fn render_mki(&mut self, out: &mut [i32; N], params: &mut [OpParams; 6], algorithm: usize, fb_buf: &mut [i32; 2], fb_shift: i32, carrier_norm: bool) {
        const LEVEL_THRESH: i32 = ENV_MAX - 100;
        let t = tables();
        let com = if carrier_norm { COM[carrier_count(algorithm)] } else { 0 };
        let orig = ALGORITHMS[algorithm & 31];
        let mut alg = orig;
        let fb_on = fb_shift < 16;
        // Algorithms 4 and 6 have a feedback loop through two/three
        // operators; Dexed renders those chains in one pass.
        if (algorithm == 3 || algorithm == 5) && fb_on {
            alg[0] = 0xc4;
        }
        let mut has = [true, false, false];
        let mut op = 0;
        while op < 6 {
            let flags = alg[op];
            let mut add = flags & OUT_BUS_ADD != 0;
            let inbus = usize::from((flags >> 4) & 3);
            let outbus = usize::from(flags & 3);
            let gain1 = if params[op].gain_out == 0 { ENV_MAX - 1 } else { params[op].gain_out };
            let mut gain2 = ENV_MAX - (params[op].level_in >> (28 - ENV_BITDEPTH));
            if orig[op] & OUT_BUS_ADD != 0 {
                // Attenuation (larger = quieter): this also weakens the
                // feedback of a carrier that feeds back, as on the DX7.
                gain2 = (gain2 + com).min(ENV_MAX);
            }
            params[op].gain_out = gain2;
            let mut skip = 0;
            if gain1 <= LEVEL_THRESH || gain2 <= LEVEL_THRESH {
                if !has[outbus] {
                    add = false;
                }
                let input_live = inbus != 0 && has[inbus];
                if !input_live && (flags & 0xc0) == 0xc0 && fb_on {
                    let (o, _) = Self::bus_ptr(out, &mut self.bus, outbus, 0);
                    match algorithm {
                        3 => {
                            mki_fb_chain::<3>(o, params, gain1, gain2, fb_buf, (fb_shift + 2).min(16), com);
                            params[1].phase = params[1].phase.wrapping_add(params[1].freq << LG_N);
                            params[2].phase = params[2].phase.wrapping_add(params[2].freq << LG_N);
                            skip = 2;
                        }
                        5 => {
                            mki_fb_chain::<2>(o, params, gain1, gain2, fb_buf, (fb_shift + 2).min(16), com);
                            params[1].phase = params[1].phase.wrapping_add(params[1].freq << LG_N);
                            skip = 1;
                        }
                        31 => mki_fb(o, &params[op], gain1, gain2, fb_buf, (fb_shift + 2).min(16), add),
                        _ => mki_fb(o, &params[op], gain1, gain2, fb_buf, fb_shift, add),
                    }
                } else {
                    let (o, input) = Self::bus_ptr(out, &mut self.bus, outbus, if input_live { inbus } else { 0 });
                    let p = &params[op];
                    let dg = dgain(gain1, gain2);
                    let mut gain = gain1;
                    let mut phase = p.phase;
                    if let Some(inp) = input {
                        for i in 0..N {
                            gain += dg;
                            let y = t.mki_sin(phase.wrapping_add(inp[i]), gain as u16);
                            o[i] = if add { o[i].wrapping_add(y) } else { y };
                            phase = phase.wrapping_add(p.freq);
                        }
                    } else {
                        for i in 0..N {
                            gain += dg;
                            let y = t.mki_sin(phase, gain as u16);
                            o[i] = if add { o[i].wrapping_add(y) } else { y };
                            phase = phase.wrapping_add(p.freq);
                        }
                    }
                }
                has[outbus] = true;
            } else if !add {
                has[outbus] = false;
            }
            params[op].phase = params[op].phase.wrapping_add(params[op].freq << LG_N);
            op += 1 + skip;
        }
    }
}

fn mki_fb(o: &mut [i32; N], p: &OpParams, gain1: i32, gain2: i32, fb_buf: &mut [i32; 2], fb_shift: i32, add: bool) {
    let t = tables();
    let dg = dgain(gain1, gain2);
    let mut gain = gain1;
    let mut phase = p.phase;
    let mut y0 = fb_buf[0];
    let mut y = fb_buf[1];
    for i in 0..N {
        gain += dg;
        let scaled_fb = (y0.wrapping_add(y)) >> (fb_shift + 1);
        y0 = y;
        y = t.mki_sin(phase.wrapping_add(scaled_fb), gain as u16);
        o[i] = if add { o[i].wrapping_add(y) } else { y };
        phase = phase.wrapping_add(p.freq);
    }
    fb_buf[0] = y0;
    fb_buf[1] = y;
}

/// Feedback through a chain of `K` operators (OP6 -> OP5 [-> OP4]), as in
/// algorithms 6 (K = 2) and 4 (K = 3). Output overwrites `o`. The last
/// operator of the chain is the carrier and gets the `com` attenuation.
fn mki_fb_chain<const K: usize>(o: &mut [i32; N], parms: &mut [OpParams; 6], gain01: i32, gain02: i32, fb_buf: &mut [i32; 2], fb_shift: i32, com: i32) {
    let t = tables();
    let mut gain = [0i32; K];
    let mut dg = [0i32; K];
    let mut phase = [0i32; K];
    for k in 0..K {
        phase[k] = parms[k].phase;
    }
    gain[0] = gain01;
    dg[0] = dgain(gain01, gain02);
    for k in 1..K {
        parms[k].gain_out = ENV_MAX - (parms[k].level_in >> (28 - ENV_BITDEPTH));
        if k == K - 1 {
            parms[k].gain_out = (parms[k].gain_out + com).min(ENV_MAX);
        }
        gain[k] = if parms[k].gain_out == 0 { ENV_MAX - 1 } else { parms[k].gain_out };
        dg[k] = parms[k].gain_out - gain[k];
    }
    let mut y0 = fb_buf[0];
    let mut y = fb_buf[1];
    for oi in o.iter_mut() {
        let scaled_fb = (y0.wrapping_add(y)) >> (fb_shift + 1);
        gain[0] += dg[0];
        y0 = y;
        y = t.mki_sin(phase[0].wrapping_add(scaled_fb), gain[0] as u16);
        phase[0] = phase[0].wrapping_add(parms[0].freq);
        for k in 1..K {
            gain[k] += dg[k];
            y = t.mki_sin(phase[k].wrapping_add(y), gain[k] as u16);
            phase[k] = phase[k].wrapping_add(parms[k].freq);
        }
        *oi = y;
    }
    fb_buf[0] = y0;
    fb_buf[1] = y;
}
