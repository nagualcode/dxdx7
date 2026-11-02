# DX7 hardware model: decisions and evidence

This file records what the DX7 VINTAGE engine changes relative to Dexed,
why, and how sure we are. Full research notes with every source and
confidence tag: [`RESEARCH.md`](RESEARCH.md).

Tags: **[V]** verified from a primary source (die analysis, firmware,
patent, bit-accurate emulator source), **[M]** measured on hardware,
**[I]** inferred, **[S]** speculation.

## Implemented

| # | Change | Evidence | Code |
|---|---|---|---|
| 1 | Core at 9.4265 MHz / 192 = 49,096.35 Hz, resampled to host | [V] service manual, Shirriff, VDX7 | `vintage::DX7_SAMPLE_RATE`, `resample.rs` |
| 2 | Carrier-count compensation COM = 0, 1024, 1664, 2048, 2432, 2688 (1024 = 6 dB) added to carrier attenuation before exp | [V] Shirriff part 4, VDX7 algorithm ROM, US 4,554,857 | `core.rs` `COM`, `render_mki` |
| 3 | Per-voice DAC: 15-bit voice word, mask 3/2/1/0 LSBs for abs >= 4096/2048/1024/below | [V] Shirriff part 5, VDX7 | `synth::dx7_dac` |
| 4 | Voices summed after conversion (analog S&H sum) | [V] Shirriff part 5, US 4,632,001 | `synth::render` |
| 5 | Zero-order hold droop | [V] S&H circuit (TC4066 + op-amp) | `resample.rs` (sinc term) |
| 6 | 5th-order output low-pass: pole 6788 Hz; pairs 11588 Hz / Q 1.017, 16062 Hz / Q 4.13 | [V] topology, [I] values from VDX7's circuit model (component values not readable in available scans) | `vintage::output_filter_response`, embedded in the FIR |
| 7 | Pitch grid 1024/octave; key code 0 / 85 / 170 steps within each minor third; firmware coarse table, fine = round(4096 log2(1+f/100)), `>>2`; fixed mode `(0x16AC + FIXED + 136 fine) >> 2` | [V] ajxs firmware v1.8 disassembly | `note::osc_freq_hw` |
| 8 | LFO / modulation update every ~132 samples (firmware 372 Hz tick) | [V] firmware timer | `synth::render` (`lfo_steps`) |
| 9 | DAC ladder gain error (Tolerance) | [I] 0.1 % resistors per VDX7 | `Engine::apply_settings` |
| 10 | Hiss about -84 dBFS | [S] no measurement published | `vintage::HISS` |

Level calibration: with carrier scaling a voice never exceeds 16,384 DAC
units; that maps to 0.25 (-12 dBFS), which makes a typical two-carrier
voice as loud as in Mark I / Dexed. Six-carrier organ voices are quieter
than in Dexed, as on the hardware.

Why the output filter lives in the resampler: its poles (11.6 and
16.1 kHz, Q 4.13) are close to the host Nyquist. A bilinear-transform IIR
at 48 kHz measures -7.6 dB at 15 kHz instead of +0.8 dB. The FIR is
designed by frequency sampling from the analog magnitude (times the ZOH
sinc and an anti-image taper), so `tests::output_filter_matches_dx7_response`
holds within 0.5 dB at 44.1, 48 and 96 kHz.

## Kept from Dexed (deliberately)

- **Detune law**: Dexed's curve fitted to measurements of a real DX7 [M]
  (±12.7 cents at C2 to ±2.5 at C7 for detune ±7). VDX7 assumes a constant
  ±1 step per unit [I]. The measurement wins until better data appears.
- **Feedback shift**: Dexed's `>> (9 - FB)`, which the Dexed author tuned
  by ear against his DX7. VDX7 uses one bit more. Unresolved [I].
- **Envelopes**: Dexed's block-rate envelope with the measured static
  timings. The EGS updates every sample in 12-bit steps with rate masks
  (0xAA, 0xEA, 0xEE, 0xFE); porting that is the next big step.

## Open / next

1. Per-sample EGS envelope (fixes Mark I attack clicks, Dexed #486).
2. VDX7's full 32-algorithm feedback/SEL table (algorithm 21's extra delay).
3. 32-level velocity quantisation (`TABLE_MIDI_VEL`).
4. Pitch EG at the firmware's 186 Hz step rate.
5. A recording of a real DX7 (sine at several levels, algorithm 32 feedback)
   to settle the feedback shift, hiss level and output gain.
