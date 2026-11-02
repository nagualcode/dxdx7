//! Switch-type parameters. Index order is part of the saved state: append
//! new variants at the end, never reorder.

use truce::prelude::*;

/// Keyboard level scaling curve (DX7 order: -LIN, -EXP, +EXP, +LIN).
#[derive(ParamEnum)]
pub enum KeyCurve {
    #[name = "-LIN"]
    NegLin,
    #[name = "-EXP"]
    NegExp,
    #[name = "+EXP"]
    PosExp,
    #[name = "+LIN"]
    PosLin,
}

#[derive(ParamEnum)]
pub enum OscMode {
    Ratio,
    Fixed,
}

#[derive(ParamEnum)]
pub enum LfoWave {
    Triangle,
    #[name = "Saw Down"]
    SawDown,
    #[name = "Saw Up"]
    SawUp,
    Square,
    Sine,
    #[name = "S&H"]
    SampleHold,
}

/// Sound engine.
/// - Modern: msfa high-resolution sine core (Dexed "Modern").
/// - Mark I: Dexed's log-sin/exp core modelled on the OPS chip.
/// - DX7 Vintage: the Mark I core run at the DX7's own sample rate, then
///   the DX7 DAC, sample-and-hold and output stage (see `dx7::vintage`).
#[derive(ParamEnum)]
pub enum EngineSw {
    Modern,
    #[name = "Mark I"]
    MarkI,
    #[name = "DX7 Vintage"]
    Vintage,
}

/// Velocity mapping. The DX7 keyboard tops out lower than a MIDI
/// controller; Dexed's "DX7" option scales 127 down to 100.
#[derive(ParamEnum)]
pub enum VelCurveSw {
    #[name = "MIDI"]
    Midi,
    #[name = "DX7 keyboard"]
    Dx7,
}

/// Output converter model (Vintage engine).
#[derive(ParamEnum)]
pub enum DacSw {
    /// No converter quantisation.
    Clean,
    /// DX7 Mark I: 12-bit DAC with the analog exponent (gain) stage.
    #[name = "DX7 Mk I"]
    Dx7,
    /// Fixed-point 12-bit (no gain ranging) - a "worst case" lo-fi option.
    #[name = "12-bit flat"]
    Flat12,
}
