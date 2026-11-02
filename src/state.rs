//! Non-parameter state saved with the DAW project: the loaded cartridge,
//! the selected program and the voice name.
//!
//! The sound itself lives in the host parameters (every VCED byte is a
//! parameter), so a project always recalls the exact voice, edits included,
//! even if the cartridge it came from is gone.

use truce::prelude::*;

use crate::cartridges;
use crate::sysex::{CART_SIZE, Cartridge};

#[derive(State, Clone)]
pub struct PatchState {
    /// Packed VMEM voice data (4096 bytes) of the current cartridge.
    pub cart: Vec<u8>,
    /// Display name of the cartridge (file name without extension).
    pub cart_name: String,
    /// Index into the built-in cartridge list, or -1 for a loaded file.
    pub builtin: i32,
    /// Selected program (0..32).
    pub program: u32,
    /// Name of the voice currently in the parameters (10 characters).
    pub voice_name: String,
}

impl Default for PatchState {
    fn default() -> Self {
        // A fresh instance holds the INIT VOICE (the parameter defaults);
        // the first built-in cartridge is ready to browse.
        let cart = cartridges::builtin(0);
        Self {
            cart: cart.data.to_vec(),
            cart_name: cartridges::BUILTIN[0].0.to_string(),
            builtin: 0,
            program: 0,
            voice_name: "INIT VOICE".into(),
        }
    }
}

impl PatchState {
    pub fn cartridge(&self) -> Cartridge {
        let mut c = Cartridge::default();
        if self.cart.len() == CART_SIZE {
            c.data.copy_from_slice(&self.cart);
        }
        c
    }
}
