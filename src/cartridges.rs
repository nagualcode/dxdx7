//! Built-in cartridges: the banks Dexed ships with (`builtin_pgm.zip`),
//! embedded in the plugin binary so they work in every host without an
//! install step.

use crate::sysex::Cartridge;

macro_rules! carts {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/carts/", $name, ".syx")))),*]
    };
}

pub static BUILTIN: &[(&str, &[u8])] = carts![
    "Dexed_01",
    "SynprezFM_01", "SynprezFM_02", "SynprezFM_03", "SynprezFM_04",
    "SynprezFM_05", "SynprezFM_06", "SynprezFM_07", "SynprezFM_08",
    "SynprezFM_09", "SynprezFM_10", "SynprezFM_11", "SynprezFM_12",
    "SynprezFM_13", "SynprezFM_14", "SynprezFM_15", "SynprezFM_16",
    "SynprezFM_17", "SynprezFM_18", "SynprezFM_19", "SynprezFM_20",
    "SynprezFM_21", "SynprezFM_22", "SynprezFM_23", "SynprezFM_24",
    "SynprezFM_25", "SynprezFM_26", "SynprezFM_27", "SynprezFM_28",
    "SynprezFM_29", "SynprezFM_30", "SynprezFM_31", "SynprezFM_32",
];

pub fn builtin(idx: usize) -> Cartridge {
    BUILTIN
        .get(idx)
        .and_then(|(_, bytes)| Cartridge::from_bytes(bytes))
        .unwrap_or_default()
}
