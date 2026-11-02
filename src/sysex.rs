//! DX7 SysEx formats: 32-voice cartridges (VMEM bulk dump, the `.syx` files
//! Dexed and the original DX7 use) and single voices (VCED).
//!
//! Byte layouts follow the DX7 MIDI implementation chart and Dexed's
//! `Cartridge::unpackProgram`, including its tolerance of out-of-range
//! bytes in third-party banks.

pub const VOICE_SIZE: usize = 155;
/// Unpacked voice plus Dexed's operator on/off byte (index 155).
pub type Voice = [u8; 156];
pub const CART_SIZE: usize = 4096;
pub const CART_SYSEX_SIZE: usize = 4104;
const VMEM_HEADER: [u8; 6] = [0xF0, 0x43, 0x00, 0x09, 0x20, 0x00];
const VCED_HEADER: [u8; 6] = [0xF0, 0x43, 0x00, 0x00, 0x01, 0x1B];

/// Yamaha checksum: two's complement of the 7-bit sum.
pub fn checksum(data: &[u8]) -> u8 {
    let mut sum: u32 = 0;
    for &b in data {
        sum = sum.wrapping_sub(u32::from(b));
    }
    (sum & 0x7F) as u8
}

/// The DX7 "INIT VOICE": one sine carrier (OP1) at full level.
pub fn init_voice() -> Voice {
    let mut v = [0u8; 156];
    for slot in 0..6 {
        let o = slot * 21;
        v[o..o + 8].copy_from_slice(&[99, 99, 99, 99, 99, 99, 99, 0]);
        v[o + 8] = 39; // break point C3
        v[o + 18] = 1; // coarse 1
        v[o + 20] = 7; // detune centre
    }
    v[5 * 21 + 16] = 99; // OP1 output level
    v[126..134].copy_from_slice(&[99, 99, 99, 99, 50, 50, 50, 50]);
    v[134] = 0; // algorithm 1
    v[135] = 0;
    v[136] = 1; // osc key sync
    v[137] = 35;
    v[141] = 1;
    v[143] = 3;
    v[144] = 24;
    v[145..155].copy_from_slice(b"INIT VOICE");
    v[155] = 0x3F;
    v
}

/// Printable 10-character voice name (Dexed's normalisation of the
/// DX7 character set: yen -> Y, arrows -> > and <).
pub fn normalize_name(raw: &[u8]) -> String {
    raw.iter()
        .take(10)
        .map(|&c| match c & 0x7F {
            92 => 'Y',
            126 => '>',
            127 => '<',
            c if c < 32 => ' ',
            c => c as char,
        })
        .collect()
}

pub fn voice_name(v: &Voice) -> String {
    normalize_name(&v[145..155])
}

pub fn set_voice_name(v: &mut Voice, name: &str) {
    let mut bytes = name.bytes().filter(|c| (32..127).contains(c));
    for i in 0..10 {
        v[145 + i] = bytes.next().unwrap_or(b' ');
    }
}

/// Clamp a value that should be `0..=max`. Out-of-range bytes in third-party
/// banks are treated as 0..255 data and rescaled, as Dexed does.
fn normparm(value: u8, max: u8) -> u8 {
    if value <= max { value } else { ((f32::from(value) / 255.0) * f32::from(max)) as u8 }
}

/// A 32-voice cartridge in packed VMEM form (128 bytes per voice).
#[derive(Clone)]
pub struct Cartridge {
    pub data: [u8; CART_SIZE],
}

impl Default for Cartridge {
    fn default() -> Self {
        let mut c = Self { data: [0; CART_SIZE] };
        let init = init_voice();
        for i in 0..32 {
            c.set_voice(i, &init);
        }
        c
    }
}

/// What a `.syx` file turned out to contain.
pub enum Loaded {
    Cart(Cartridge, LoadQuality),
    Voice(Voice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadQuality {
    /// Proper VMEM dump with a matching checksum.
    Ok,
    /// VMEM dump, checksum mismatch (common in old banks; still loaded).
    BadChecksum,
    /// No SysEx framing found; first 4096 bytes taken as raw voice data.
    Raw,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    TooSmall,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::TooSmall => write!(f, "not a DX7 voice or cartridge (file too small)"),
        }
    }
}

/// Parse a `.syx` (or raw) file.
pub fn load(bytes: &[u8]) -> Result<Loaded, LoadError> {
    // Single voice VCED dump: F0 43 0n 00 01 1B <155> <chk> F7.
    if bytes.len() >= 163 && bytes[0] == 0xF0 && bytes[1] == 0x43 && bytes[3] == 0x00 && bytes[4] == 0x01 && bytes[5] == 0x1B {
        let mut v = [0u8; 156];
        v[..VOICE_SIZE].copy_from_slice(&bytes[6..6 + VOICE_SIZE]);
        let max = vced_max();
        for (b, m) in v.iter_mut().zip(max.iter()) {
            *b = normparm(*b & 0x7F, *m);
        }
        v[155] = 0x3F;
        return Ok(Loaded::Voice(v));
    }
    // Scan for a VMEM bulk dump anywhere in the stream (files often hold
    // several SysEx messages, e.g. a performance dump first).
    let mut pos = 0;
    while pos + CART_SYSEX_SIZE <= bytes.len() {
        let b = &bytes[pos..];
        if b[0] == 0xF0 && b[1] == 0x43 && (b[2] & 0xF0) == 0x00 && b[3] == 0x09 && b[4] == 0x20 && b[5] == 0x00 {
            let mut c = Cartridge { data: [0; CART_SIZE] };
            c.data.copy_from_slice(&b[6..6 + CART_SIZE]);
            let q = if checksum(&c.data) == b[6 + CART_SIZE] { LoadQuality::Ok } else { LoadQuality::BadChecksum };
            return Ok(Loaded::Cart(c, q));
        }
        if b[0] == 0xF0 {
            // Skip this message.
            match b.iter().position(|&x| x == 0xF7) {
                Some(end) => pos += end + 1,
                None => break,
            }
        } else {
            pos += 1;
        }
    }
    // Raw voice data (some banks are stored without SysEx framing).
    if bytes.len() >= CART_SIZE {
        let start = if bytes.len() >= CART_SYSEX_SIZE && bytes[0] == 0xF0 { 6 } else { 0 };
        let mut c = Cartridge { data: [0; CART_SIZE] };
        c.data.copy_from_slice(&bytes[start..start + CART_SIZE]);
        return Ok(Loaded::Cart(c, LoadQuality::Raw));
    }
    Err(LoadError::TooSmall)
}

/// Maximum value of each VCED byte (0..=154).
pub fn vced_max() -> [u8; VOICE_SIZE] {
    let op = [99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 3, 3, 7, 3, 7, 99, 1, 31, 99, 14];
    let mut m = [0u8; VOICE_SIZE];
    for slot in 0..6 {
        m[slot * 21..slot * 21 + 21].copy_from_slice(&op);
    }
    m[126..145].copy_from_slice(&[99, 99, 99, 99, 99, 99, 99, 99, 31, 7, 1, 99, 99, 99, 99, 1, 5, 7, 48]);
    for b in &mut m[145..155] {
        *b = 127;
    }
    m
}

impl Cartridge {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        match load(bytes).ok()? {
            Loaded::Cart(c, _) => Some(c),
            Loaded::Voice(_) => None,
        }
    }

    pub fn name(&self, idx: usize) -> String {
        normalize_name(&self.data[idx * 128 + 118..idx * 128 + 128])
    }

    /// Unpack voice `idx` (0..32) into a 156-byte patch (all operators on).
    pub fn voice(&self, idx: usize) -> Voice {
        let bulk = &self.data[idx * 128..idx * 128 + 128];
        let mut r = [0u8; 156];
        for op in 0..6 {
            let b = &bulk[op * 17..op * 17 + 17];
            let o = op * 21;
            for i in 0..11 {
                r[o + i] = normparm(b[i] & 0x7F, 99);
            }
            let curves = b[11] & 0x0F;
            r[o + 11] = curves & 3;
            r[o + 12] = (curves >> 2) & 3;
            let det_rs = b[12] & 0x7F;
            r[o + 13] = det_rs & 7;
            let kvs_ams = b[13] & 0x1F;
            r[o + 14] = kvs_ams & 3;
            r[o + 15] = (kvs_ams >> 2) & 7;
            r[o + 16] = normparm(b[14] & 0x7F, 99);
            let fc_mode = b[15] & 0x3F;
            r[o + 17] = fc_mode & 1;
            r[o + 18] = (fc_mode >> 1) & 0x1F;
            r[o + 19] = normparm(b[16] & 0x7F, 99);
            r[o + 20] = normparm((det_rs >> 3) & 0x0F, 14);
        }
        for i in 0..8 {
            r[126 + i] = normparm(bulk[102 + i] & 0x7F, 99);
        }
        r[134] = normparm(bulk[110] & 0x1F, 31);
        let oks_fb = bulk[111] & 0x0F;
        r[135] = oks_fb & 7;
        r[136] = oks_fb >> 3;
        r[137] = normparm(bulk[112] & 0x7F, 99);
        r[138] = normparm(bulk[113] & 0x7F, 99);
        r[139] = normparm(bulk[114] & 0x7F, 99);
        r[140] = normparm(bulk[115] & 0x7F, 99);
        let lpms_lfw_lks = bulk[116] & 0x7F;
        r[141] = lpms_lfw_lks & 1;
        r[142] = normparm((lpms_lfw_lks >> 1) & 7, 5);
        r[143] = (lpms_lfw_lks >> 4) & 7;
        r[144] = normparm(bulk[117] & 0x7F, 48);
        for i in 0..10 {
            r[145 + i] = bulk[118 + i] & 0x7F;
        }
        r[155] = 0x3F;
        r
    }

    /// Pack a voice into slot `idx`.
    pub fn set_voice(&mut self, idx: usize, v: &Voice) {
        let bulk = &mut self.data[idx * 128..idx * 128 + 128];
        for op in 0..6 {
            let o = op * 21;
            let b = &mut bulk[op * 17..op * 17 + 17];
            b[..11].copy_from_slice(&v[o..o + 11]);
            b[11] = (v[o + 11] & 3) | ((v[o + 12] & 3) << 2);
            b[12] = (v[o + 13] & 7) | ((v[o + 20] & 15) << 3);
            b[13] = (v[o + 14] & 3) | ((v[o + 15] & 7) << 2);
            b[14] = v[o + 16];
            b[15] = (v[o + 17] & 1) | ((v[o + 18] & 31) << 1);
            b[16] = v[o + 19];
        }
        bulk[102..110].copy_from_slice(&v[126..134]);
        bulk[110] = v[134] & 31;
        bulk[111] = (v[135] & 7) | ((v[136] & 1) << 3);
        bulk[112] = v[137];
        bulk[113] = v[138];
        bulk[114] = v[139];
        bulk[115] = v[140];
        bulk[116] = (v[141] & 1) | ((v[142] & 7) << 1) | ((v[143] & 7) << 4);
        bulk[117] = v[144];
        bulk[118..128].copy_from_slice(&v[145..155]);
    }

    /// Full 4104-byte VMEM SysEx message.
    pub fn to_sysex(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CART_SYSEX_SIZE);
        out.extend_from_slice(&VMEM_HEADER);
        out.extend_from_slice(&self.data);
        out.push(checksum(&self.data));
        out.push(0xF7);
        out
    }
}

/// Single-voice VCED SysEx message (163 bytes).
pub fn voice_to_sysex(v: &Voice) -> Vec<u8> {
    let mut out = Vec::with_capacity(163);
    out.extend_from_slice(&VCED_HEADER);
    out.extend_from_slice(&v[..VOICE_SIZE]);
    out.push(checksum(&v[..VOICE_SIZE]));
    out.push(0xF7);
    out
}
