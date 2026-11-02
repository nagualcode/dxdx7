# Yamaha DX7 (Mark I) hardware facts for emulation

How sure each item is: **[V]** verified from a primary source (die analysis, firmware disassembly, patent, or bit-accurate emulator source that I read directly). **[M]** measured on real hardware by a named person. **[I]** inferred or assumed by a reverse-engineer and not yet confirmed. **[S]** my own speculation.

Main primary sources:
- Ken Shirriff's OPS die analysis, parts 1–6: righto.com/2021/11/reverse-engineering-yamaha-dx7.html, …/2021/11/reverse-engineering-yamaha-dx7_28.html (exp ROM), …/2021/12/yamaha-dx7-reverse-engineering-part-iii.html (log-sine ROM), …/2021/12/yamaha-dx7-chip-reverse-engineering.html (algorithms), …/2022/02/yamaha-dx7-chip-reverse-engineering.html (output), …/2022/02/yamaha-dx7-chip-reverse-engineering_7.html (registers)
- VDX7, a firmware-running, cycle-interleaved emulator: github.com/chiaccona/VDX7. I read `src/OPS.h`, `src/EGS.h`, `src/filter.h`, `src/Synth.cc`, `README.code.md` and `README.FAQ.md`.
- ajxs, annotated v1.8 firmware disassembly: github.com/ajxs/yamaha_dx7_rom_disassembly (`yamaha_dx7_rom_v1.8.asm`)
- Raph Levien's MSFA wiki: github.com/google/music-synthesizer-for-android/blob/master/wiki/Dx7Hardware.wiki and Dx7Envelope.wiki
- Dexed source (`source/engine/EngineMkI.cpp`, `source/msfa/*`) and issues #88, #138, #146, #291, #486, #510, #511, PR #520/#521

---

## 1. Clock, sample rate, multiplexing

- **Master crystal: 9.4265 MHz [V].** VDX7 `Synth.cc` uses 9.4265e6. The OPS and EGS clock is 9.4265/2 = **4.71325 MHz**; Shirriff gives "4.7 MHz".
- **Sample rate = 4.71325 MHz / 96 = 49,096.35 Hz [V].** The service manual and Shirriff both give 49.096 kHz.
- **Multiplexing [V]:** 16 voices × 6 operators = 96 slots per sample, one operator result per clock.
  - Operators run in the order **OP6 → OP1**. For a given operator, all 16 voices run back to back, so a voice's consecutive operators are 16 clocks apart. Each operator is pipelined over about 16 clocks.
  - The 16 per-voice sums leave the chip one at a time at **16 × 49,096 = 785.5 kHz**.
  - Voice output order according to Yamaha's technical analysis, as used by VDX7: `0,8,4,12,2,10,6,14,1,9,5,13,3,11,7,15`.
- **The CPU is the HD63B03RP [V].** Its E clock is 9.4265/8 = 1.1783 MHz. The OCF timer tick is `SYSTEM_TICK_PERIOD = 3140` E cycles (ajxs).
  - That is ≈2.67 ms nominal. The measured LFO top rate implies ≈**372 Hz** including interrupt latency [I].
  - The LFO and amplitude/pitch modulation are written to the EGS **every tick (~372 Hz)** [V].
  - The pitch EG runs **every second tick (~186 Hz)**, and is skipped on any tick where MIDI input is pending [V].

## 2. OPS (YM21280) internals

**Log-sine ROM [V] (Shirriff part III):**
- Quarter wave with **1024 entries, 14-bit output**, stored delta-compressed in 5,344 bits.
- Entry value: `round(-log2(sin((i+0.5)/1024·π/2))·1024)`. The hardware rounding is effectively `int(y·1024+0.5002)`.
- Input phase is 12 bits:
  - bit 10 mirrors the index (`~i`),
  - bit 11 is the sign (kept as a separate flag, so the waveform is sign-magnitude).

**Exp ROM [V]:**
- 1024 entries, value `round(2^(i/1024)·2048)`, which is 12 bits with an implicit leading 1.
- Input is Q4.10 (14 bits). There are two copies of the ROM:
  - frequency path: `exp22(x) = round(2^frac·2048) << int >> 5` (22-bit output);
  - signal path: `exp14(x) = exp22(x) >> 8`, max 16,372.
- When int = 0 the signal path outputs 0, which is a hard floor.

**Phase accumulator:**
- The pitch input is **14-bit log2 frequency, Q4.10 (1024 steps/octave = 1.172 cents)** [V].
- The increment is `exp22(pitch)`, up to 4,193,280 [V].
- VDX7 wraps the phase at **2^23** and uses the top 12 bits (`phase >> 11`) as the sine address [V in VDX7, consistent with the top pitch being exactly Nyquist].
- The patent US 4,554,857 says "96-stage/22-bit shift register" [V patent]. Treat the 23-bit modulus as the working model.
- Frequency = `exp22(p)·49096/2^23` Hz.
- At octave int ≥ 5 the increment keeps the full 12-bit mantissa. Below about 12 Hz (int < 5) the low bits are truncated; at int = 0 there are only 64 distinct increments per octave. This matters for fixed-frequency sub-audio operators.

**Operator datapath [V; the env<<2 alignment is I]:**
```
phi    = (phase >> 11) + mod                 // 12-bit, wraps
ls     = logsin(phi)                         // 14-bit + sign
ls    += env12 << 2                          // 12-bit EGS attenuation → 14-bit (VDX7 assumption)
ls    += COM[n]                              // carrier-count attenuation, see below
if ls >= 0x4000: ls = 0x3FFF                 // clamp
out    = ±exp14(ls ^ 0x3FFF)                 // ≤ 16372, i.e. 15-bit two's complement incl. sign
```
- **Modulation scaling [V]:** a modulator's linear output (≤ 16,372) is added directly to the 12-bit phase (4096 per cycle). Full-scale modulation is therefore **±4 cycles = ±8π rad** of phase deviation. Dexed MkI matches this: output `<<13` against a 2^24 phase cycle gives 4 cycles.
- **COM (carrier normalisation) [V, Shirriff part 4 + VDX7 algo ROM]:**
  - Each carrier gets `log2(N)` added in the log domain, where N is the number of carriers. The values are `00.000, 01.000, 01.101, 10.000, 10.011, 10.101` in Q2.3 (×128 in Q4.10), so each carrier is scaled by about 1/N.
  - Because the scaling is applied before the exp, it also reduces the feedback value of a carrier that feeds back. In algorithm 32, OP6's self-feedback is about 6× weaker than when OP6 is a modulator. **This explains Dexed issue #146** ("Alg 32 feedback too strong vs real DX7").
  - Dexed does not apply this scaling at all; it sums carriers raw.
- **Feedback [V that it averages; shift I]:**
  - The patent and Shirriff agree: the feedback is the sum of the feedback operator's last two outputs, divided by 2, then scaled by a power of two ("anti-hunting" averaging, Tomisawa).
  - VDX7 implements `(f1+f2) >> (1 + (7 − FB))`. So FB = 7 means the plain average, and each FB step below 7 halves it [I].
  - Dexed MkI uses `(y0+y) >> (8 − FB + 1)`, one bit weaker than VDX7 for the same amplitude scale [V from code]. The two disagree; validate against a recording, e.g. the issue #146 sample.
  - FB = 0 must disable feedback entirely. VDX7 lets FB = 0 give `>> 8`; I think that is likely a bug [S].
- **Multi-operator feedback loops [V, VDX7 algo ROM, Shirriff "SEL" values]:** the feedback register always latches the output of one specific operator (flag A) and feeds it, with averaging, into another operator in the next sample.
  - Algorithm 4: OP4's output is fed back to OP6 (loop 6→5→4→6).
  - Algorithm 6: OP5's output is fed back to OP6.
  - The full SEL/A/C/D/COM table for all 32 algorithms is in VDX7 `OPS.h`. Port it verbatim.
  - Algorithm 21 uses an extra one-operator delay buffer (SEL3).
- **Intra-voice sum:** carriers are summed in a "MREN" register. Because each carrier is ≤ 16,372/N, the sum fits in **15 bits signed**; there is no saturation path [V/I].
- **Phase reset:** key sync (OPS mode register) resets all six phases to 0 at key-on. With sync off the phases free-run [V].

## 3. EGS (YM21290) envelope and amplitude

No die analysis of the EGS exists. Everything below comes from MSFA measurements, VDX7 and the firmware.

- **Envelope output: 12-bit log attenuation [V interface, MSFA].** One unit is 0.0235 dB (256 units per 6.02 dB, Q8 log2), giving about 96 dB range. The minimum level used is attenuation 0xFF0.
- **Levels [MSFA, firmware]:**
  - EGS target = `(L<<6) + (outlevel<<4)`, clamped to 0xFF0.
  - Patch level 0–99 maps to actual level via the MSFA mapping: 0–5 → 2l; 5–16 → 5+l; 17–20 → 4+l; else 14+(l>>1).
  - Output level 0–19 uses the table `0,5,9,13,17,20,23,25,27,29,31,33,35,37,39,41,42,43,45,46`; 20–99 maps to 28+l.
- **Rates [MSFA, M]:**
  - `qrate = (rate·41)>>6`, plus rate scaling, clamped to 63.
  - The single global envelope clock ticks once per sample (49,096 Hz).
  - For qrate < 44: one step every `2^(11 − qrate/4)` samples, gated by the fractional masks `0xAA, 0xEA, 0xEE, 0xFE` for `qrate & 3`.
  - For qrate ≥ 48: the step size is `1 << (qrate/4 − 11)` every sample.
  - Decay is linear in dB, roughly `0.2819·2^(qrate/4)·(1+0.25·(qrate%4))` dB/s.
- **Attack [MSFA, VDX7]:**
  - The level first jumps to attenuation 0x94C, which is 1716 steps (≈40 dB) above the minimum.
  - Each tick then subtracts `((att>>8)+2) << pshift`. That is a slope of 2–11 per tick, faster when quieter, which gives the near-linear-amplitude, concave attack shape.
- **Envelope "delay" quirk [I, VDX7 from Pascal Gauthier's measurements]:**
  - If both the current and the previous EG level are below patch value 20 (i.e. ≥ 40 internal units), the EGS inserts a delay worth 479 decay steps.
  - The stage-1 delay is "compressed" to 15 steps if the previous stage-1 delay did not finish (speculative).
  - Patches that use it include WATER GDN and CHIMES.
- **Rate scaling [I]:** VDX7 uses `rs = round(min(27, max(0, (voicePitch>>8) − 16)) · RS/7)` in qrate units, fitted to the manual's graph. Dexed uses `(RS·min(31, max(0, note/3 − 7)))>>3`. They differ slightly.
- **Amplitude modulation [I, VDX7]:** the firmware sends an 8-bit log AM value about every 2.7 ms. The EGS adds `AM << AMS` for AMS 1–3 to the attenuation (shifts ×2, ×4, ×8). At full AM depth this is 12, 24 and 48 dB. Dexed uses the ratios 0, 0.26, 0.43, 1.0.
- **Velocity [V firmware]:** the internal velocity table has 32 entries (`TABLE_OP_VOLUME_VELOCITY_SCALE`: 4, 0x0C, 0x15, 0x1E, …, 0x82). Incoming MIDI velocity is quantised to 32 levels (`TABLE_MIDI_VEL`), and the transmitted range is 16–118 (VDX7 FAQ). Users report the keybed rarely exceeds about 109–119 [M, Dexed #138].

## 4. DAC and output stage

- **Per-voice conversion with analog summation [V, Shirriff part V; patent family US 4,539,883 / 4,632,001].** The chip does **not** sum the 16 voices digitally. Each voice's 15-bit value is converted separately at 785.5 kHz and the voices are summed by the sample-and-hold and low-pass filter. Quantisation therefore happens **per voice**.
- **Quasi-floating-point format [V]:**
  - 15-bit two's complement goes to a **12-bit DAC (Rohm BA9221**, two's-complement input, current output).
  - The chip shifts left by 0–3 according to the count of leading sign bits:
    - |v| < 1024 (≥5 leading zeros): shift 3, full resolution;
    - < 2048: shift 2, the lowest bit is lost;
    - < 4096: shift 1, 2 bits lost;
    - otherwise shift 0, 3 bits lost.
  - An R-2R ladder (TC4066 switches, IC41) divides the analog output by 1, 2, 4 or 8. The result is effectively **sign + 11-bit mantissa + 2-bit exponent**.
  - Shirriff notes that Yamaha deliberately leaves the top two sign bits unused.
  - The ladder resistors are 0.1% parts according to VDX7 [I], so each unit has a small gain mismatch between exponent ranges. Plogue OPS7 offers a "DAC error" control for this.
- **Master volume [V]:** the DAC reference comes from an 8-level multiplexer. This is the MIDI volume, a 3-bit value written via `TABLE_MIDI_VOLUME = 0,4,2,6,1,5,3,7` (bit-reversed).
- **Sample-and-hold [V]:** two S/H circuits alternate (TC4066, IC52, op-amp buffer) and the last two samples are summed. This has no audible effect.
- **Reconstruction filter [V topology, numbers from VDX7's circuit-derived model]:**
  - A passive 1-pole stage followed by two Sallen-Key stages, giving a **5th-order low-pass**. The service manual mentions an LF356 (IC40) in this area (forum repair report).
  - I converted VDX7's z-domain coefficients (fs = 785,536 Hz) to the analog prototype:
    - real pole **6,788 Hz**;
    - pole pair **11,588 Hz, Q 1.017**;
    - pole pair **16,062 Hz, Q 4.13**.
  - Computed magnitude response: 0 dB to 1 kHz; −0.3 dB at 5 kHz; +0.06 at 12 kHz; **+0.56 at 14 kHz; +0.80 at 15 kHz**; −0.12 at 16 kHz; **−3.0 at 17 kHz**; −6.9 at 18 kHz; **−14.2 at 20 kHz**; −20 at 22 kHz; **−26.6 at 24.55 kHz (Nyquist)**; −37 at 30 kHz; −61 at 49 kHz.
  - VDX7 notes that without the passive pole loading the first stage, the two Sallen-Key stages would peak about 12 dB at 16 kHz.
- **Not found:** component values, output gain, DC-blocking corner, measured noise floor in dB, and line versus headphone differences. Anecdotally the DX7 has "very high noise output", a "slightly gunky, muffled output jack", and OPS7 offers an optional recorded noise loop. Treat any number here as unknown.

## 5. Pitch and tuning (firmware v1.8 + EGS)

- **Resolution:** 1024 steps/octave (1.172 cents) at the EGS. The firmware works with 4096/octave and the EGS takes `>>2` [V].
- **Key → pitch [V, `VOICE_CONVERT_NOTE_TO_LOG_FREQ`]:**
  - A high byte comes from a 128-entry table that holds 3 semitones per 4 units (a 16-slot-per-octave key code). The low byte repeats that byte's low two bits.
  - Result: semitones inside each minor-third group come out at **0, 99.61, 199.22 cents** (85, 170, 256 EGS units). **Every 2nd and 3rd semitone is 0.39 and 0.78 cents flat of equal temperament.** This is a real, systematic hardware tuning offset.
- **Master tune [V]:** a 16-bit offset of 0…0x1FE with default 0x100, in 4096/octave units. That is **±75 cents**, input in steps of 2 units (0.59 cents) but effective resolution 1.17 cents after the `>>2`.
- **Ratio mode [V]:**
  - op pitch = `(COARSE_TAB[c] + FINE_TAB[f] + 0x232C) >> 2`.
  - Coarse table (4096/oct): `F000,0,1000,195C,2000,2528,295C,2CEC,3000,32B8,3528,375A,395C,3B34,3CEC,3E84,4000,4168,42B8,43F8,4528,4648,475A,4860,495C,4A4C,4B34,4C14,4CEC,4DBA,4E84,4F44`.
  - The fine table is 100 entries, log2(1+f/100)·4096.
  - I computed the deviation from exact ratios: ×5 **+0.41 c**, ×7 +0.32, ×11 −0.54, ×13 −0.29, ×17 +0.51, ×29 −0.67. The worst coarse+fine combination is 1.5 cents.
  - Because the result is a log-domain sum, every operator frequency is `2^(n/1024)`. Integer ratios like 5:1 are **not exactly harmonic**, which causes slow phase drift and beating between partials.
- **Fixed mode [V]:**
  - `(0x16AC + FIXED_TAB[c&3] + fine·136) >> 2`, with FIXED_TAB = 0, 0x3526, 0x6A4C, 0x9F74 (×1, 10, 100, 1000 Hz).
  - Each fine step is 1/100 decade.
  - The base value gives 1.0067 Hz, not 1 Hz, because of the int-1 exp truncation.
- **Detune:** the firmware sends a raw ±7 (sign-magnitude) to the EGS [V]. How the EGS scales it is **unresolved**:
  - VDX7 adds it as ±7 log steps (constant ±8.2 cents) [I].
  - Pascal Gauthier measured a real DX7 [M, Dexed #88], giving ±cents at detune ±7:

    | Note | C2 | C3 | C4 | C5 | C6 | C7 |
    |---|---|---|---|---|---|---|
    | Detune ±7 (cents) | ±12.7 | ±10.5 | ±9.3 | ±7.2 | ±4.7 | ±2.5 |

  - That is roughly −2 cents per octave: neither constant Hz nor constant cents.
  - Dexed fits it with `0.0209·exp(−0.396·log2f)/7 · logfreq`.
  - In fixed mode Dexed uses 13457/2^24 octave per step.
- **Pitch EG [V]:** levels go through `TABLE_PITCH_EG_LEVEL` (0–99 → 0–255, with 50 → 0x80 at centre, and steeper ends). Rates go through `TABLE_PITCH_EG_RATE`. The EG is linear in log pitch, stepped at about 186 Hz, and is one global parameter set for all voices.
- **LFO [V firmware, M]:**
  - One global software LFO with a 16-bit phase, updated at about 372 Hz.
  - Increment = s·11 for scaled speed s < 160, else `s·((s−160)/4+11)`.
  - Measured frequencies on a TX7 [M, Dexed #291] run from 0.0625 Hz (speed 0) through 7.99 Hz (speed 50) to **49.26 Hz (speed 99)**. The full 100-value table is in Dexed `lfo.cc`.
  - Sine uses a 64-entry, 7-bit quarter-wave table.
  - **At 49 Hz the LFO has only about 7.5 steps per cycle**, so high-rate vibrato and tremolo are audibly staircased.
- **PMS table [V]:** `0,10,20,33,55,92,153,255` (/255).

## 6. What emulations miss, according to comparisons

- **The aliasing pattern depends on the sample rate.** The hardware runs non-band-limited at 49,096 Hz, so images fold about 24.55 kHz. Running at 44.1 or 48 kHz folds at a different place, so the inharmonic alias tones land elsewhere. Dexed #510 is an open request for the native rate. Polynominal's side-by-side calls the Mark I "dirtier, aliased, more bass" than the DX7II.
- **Per-voice floating-point quantisation (Dexed #511).** Loud voices have a 3-LSB-coarser step, about 12-bit quality, and the noise floor rises and falls with each voice's level. Quiet tails keep their resolution, but are still integer-quantised by `exp14`, so a −70 dB tail is a few-LSB staircase sine. Dexed PR #520/521 (12-bit, 3-bit exponent) was closed unmerged; the maintainer plans a MkI-only version.
- **Output filter:** a +0.8 dB bump at 15 kHz and steep roll-off (−14 dB at 20 kHz). Dexed is flat to Nyquist.
- **Carrier normalisation (COM)** changes levels between algorithms and the strength of algorithm-32 feedback (Dexed #146).
- **Detune scaling** (Dexed #88), **LFO top speed** (was 2× too slow in Dexed until #481), **velocity curve and 32-level quantisation**, and **envelope clicks** (MkI clicks more than hardware, #486; Dexed updates the EG every 64 samples with linear interpolation, while the hardware steps every sample).
- **Residual output:** with algorithms 16–18 the hardware reportedly never reaches total silence (Plogue/KVR user report, unverified).
- **DX7II/TX802** use new chips (YM2604/YM3609 per Vosmaer) and a roughly 15–16-bit linear DAC (forum claims, which conflict). They are described as cleaner, with less aliasing and modulation noise; Yamaha marketing claimed about 30 dB better S/N. No published spectra were found.

## 7. Patents

- **US 4,018,121** (Chowning, Stanford, 1977): FM synthesis.
- **US 4,249,447** (N. Tomisawa, Nippon Gakki, 1981): feedback FM and the **averaging "anti-hunting"** filter (Figs. 7–8).
- **US 4,554,857** (T. Nishimoto, filed 1983, granted 1985): the DX7 architecture. It covers 96-slot time sharing in OP6→OP1 order, a 22-bit/96-stage phase shift register, log-sine table plus envelope addition plus log→linear conversion, a carrier-count level-compensation table, two-sample feedback averaging with `>>1` and then a feedback shift, and a 16-stage output shift register.
- **US 4,539,883** (M. Chihana, filed 1983, granted 1985): time-division D/A with a 10-bit mantissa and 3-bit exponent, analog divider, and per-channel S/H. This is a related design, not the exact DX7 scheme.
- **US 4,632,001**: a shared TDM DAC with per-channel analog memories.
- I found no public patent specific to the EGS.

## 8. Dexed `EngineMkI`

- **Matches the hardware:** a 1024-entry 10-bit-index log-sine table with the same formula; a 1024-entry exp table, 12-bit with an implicit 1; sign-magnitude output; the same ±4-cycle modulation scale; two-sample feedback averaging; explicit loops for algorithms 4 (`compute_fb3`) and 6 (`compute_fb2`).
- **Differs from the hardware:**
  - It runs at the host sample rate.
  - The EG is interpolated per 64-sample block and the envelope uses 14 bits.
  - There is no COM normalisation and no per-voice DAC quantisation.
  - There is no output filter (the optional UI filter is generic).
  - Its feedback shift is one bit weaker than VDX7's.
  - Detune comes from a fitted formula.
  - The algorithm 4/6 loops use a constant per-block gain for the inner operators.
- **Open issues:** #486, #510, #511, #146, #88.
- The maintainer says the MkI feedback was "tuned" by ear against his DX7.

---

## Concrete implementation recommendations

1. **Run the core at 49,096.354 Hz** (9.4265 MHz / 192) with integer arithmetic. Resample to the host rate with a high-quality band-limited SRC (pass-band to about 20 kHz, ≥ 100 dB stop-band). This reproduces the hardware's alias positions exactly. [V]
2. **Phase:** use a 23-bit accumulator. The increment is `exp22(p14)`, where `exp22(x) = (round(2^((x&1023)/1024)·2048) << (x>>10)) >> 5`. Address the sine with `(phase>>11) + mod` as 12 bits. [V/I]
3. **Log-sine:** 1024 × 14-bit, `int(−log2(sin((i+0.5)π/2048))·1024 + 0.5002)`. Bit 10 mirrors the index; bit 11 is the sign.
4. **Amplitude:** `ls + (env12<<2) + COM`, clamp at 0x3FFF, then `out = ±(exp22(ls^0x3FFF)>>8)` (≤ 16,372). COM values are 0, 1024, 1664, 2048, 2432, 2688 for 1–6 carriers, applied to carriers only via the algorithm ROM (copy VDX7's 32×6 SEL/A/C/D/COM table).
5. **Feedback:** `(f1+f2) >> (8 − FB)` for FB 1–7 (equivalent to `>>(1+7−FB)`), and 0 when FB = 0. Take the value from the operator that the A flag latches (OP4 in algorithm 4, OP5 in algorithm 6), post-COM. Keep one-bit-weaker as a switch until checked against a recording. [I]
6. **Per-voice DAC:** quantise each voice's 15-bit sum separately before mixing:
   - |v| ≥ 4096: `v & ~7`;
   - ≥ 2048: `v & ~3`;
   - ≥ 1024: `v & ~1`;
   - else unchanged.
   - Mask the magnitude, mirroring VDX7. Optionally add per-unit exponent-gain errors of about ±0.1% on the ÷2, ÷4 and ÷8 ranges. Then mix the 16 voices as a plain sum and scale by 1/2^15 per voice.
7. **Output filter:** realise the analog prototype `H(s) = ωa/(s+ωa) · Π ω0²/(s²+(ω0/Q)s+ω0²)` with fa = 6,788 Hz, (f0, Q) = (11,588 Hz, 1.017) and (16,062 Hz, 4.13). Either:
   - apply it at 49,096 Hz via bilinear transform with pre-warping, checking the response against the table in §4 (+0.8 dB at 15 kHz, −3 dB at 17 kHz, −14 dB at 20 kHz); or
   - run VDX7's exact 785.5 kHz biquads on the TDM voice stream (16× cost).
8. **EGS:**
   - Use a per-sample 12-bit attenuation EG with a global sample counter. For qrate < 44, `nshift = 11 − qrate/4` with masks `{0xAA,0xEA,0xEE,0xFE}[qrate&3]`; for qrate ≥ 48, `pshift = qrate/4 − 11`.
   - Attack: jump to 0x94C, then step `((att>>8)+2) << pshift`.
   - Target: `(L<<6)+(OL<<4)`, max 0xFF0.
   - Implement the 479-step delay quirk.
   - Do **not** interpolate per block.
9. **AM:** add `AM8 << AMS` (AMS 1–3) to the attenuation, refreshed and held every 132 samples at 49,096 Hz, matching the ≈372 Hz firmware tick (49,096/372 ≈ 132).
10. **Pitch:**
    - Keep everything in 1024/octave integer units.
    - Key pitch uses the firmware key-code method, which gives the 0 / −0.39 / −0.78-cent pattern.
    - Ratios come from the firmware coarse/fine tables `>>2`; fixed mode from `0x16AC + FIXED + 136·fine`; master tune ±256/4 units.
    - Update the LFO and pitch modulation every ~132 samples and the pitch EG every ~264 samples (held, not interpolated).
    - Use the firmware LFO increment formula and the 64-entry 7-bit sine table.
11. **Detune:** offer two models. VDX7's ±1 log step per unit (1.17 c), or Gauthier's measured curve (≈ ±12.7 c at C2 falling ≈ 2 c/oct to ±2.5 c at C7 for detune ±7). Default to the measured curve, which is empirical from real hardware.
12. **Velocity:** quantise to 32 levels using `TABLE_MIDI_VEL` when the input is MIDI, then the firmware's 32-entry operator-volume table.
13. **Noise and hiss:** there is no published measurement. If added, use optional white noise at about −80 to −85 dBFS (pure speculation [S]) after the filter, so the default stays clean.

**Biggest open uncertainties:** the feedback shift per level (one-bit ambiguity), the EGS detune law, the env<<2 alignment, the delay-compression rule, absolute output level and noise, and the analog component values (the service-manual schematics are scanned images; the archive.org copy I found lacks pages 21–35).
