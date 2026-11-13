# Phase 0 findings: Corsair K95 RGB G key capture

Status: **PASSED**. Date: 2026-11-11. Machine: CW03, Windows 11 Pro 22631.

Phase 0 asked one question: can we read the 18 G keys without Corsair software,
without breaking normal typing, and without fighting SignalRGB. The answer is
yes, and on better terms than the plan assumed.

## Headline

**No write to the keyboard is needed.** The G keys already emit on a vendor HID
input pipe with nothing sent from us. The application can be purely read only.

This removes three things the plan had budgeted for:

- The SignalRGB interleaving risk. We never open the command channel, so there
  is nothing to collide with.
- The keyboard freeze failure mode. That risk came from leaving keys routed to
  the Corsair pipe with no reader. We never change the routing, so there is no
  device state to restore on exit, crash, panic or logoff.
- The "sign off before the first write" gate. There is no write to approve.

## Hardware

| Property | Value |
| --- | --- |
| Device | Corsair K95 RGB Gaming Keyboard (original 2014, 18 G keys) |
| USB IDs | VID `0x1B1C`, PID `0x1B11` |
| iCUE | not installed, not required |
| Lighting | driven by SignalRGB, untouched by us |

## HID collections

Enumerated twice by independent implementations (a PowerShell script using
`HidP_GetCaps`, and the Rust probe's own hand written FFI). Both agreed exactly.

| Collection | Usage page / usage | Input | Output | Feature | Role |
| --- | --- | --- | --- | --- | --- |
| `MI_00&COL01` | `0x0001 / 0x0006` | 21 | 2 | 0 | keyboard, **OS owned** |
| `MI_00&COL02` | `0x000C / 0x0001` | 5 | 0 | 0 | consumer control (media) |
| `MI_00&COL03` | `0xFFC0 / 0x0002` | 64 | 0 | 0 | **vendor input pipe, G keys** |
| `MI_00&COL04` | `0xFFC2 / 0x0003` | 64 | 0 | 0 | vendor input, silent |
| `MI_01` | `0xFFC2 / 0x0004` | 65 | 65 | 65 | vendor command channel |

`MI_01` is the only writable collection, at 65 bytes, which is ckb-next's
64 byte payload plus a report ID. That is the channel SignalRGB must use. It is
a different collection from the one we read, which is why coexistence is free
rather than negotiated.

## Access results

Measured, not assumed:

- `0x0001/0x0006` (keyboard): `CreateFileW` returns **ACCESS_DENIED (5)**.
  Windows claims keyboard top level collections exclusively. We cannot open the
  pipe that carries normal typing even if we wanted to. It also recorded 0
  reports across the whole run.
- All four other collections opened for `GENERIC_READ` with
  `FILE_SHARE_READ | FILE_SHARE_WRITE` while SignalRGB was running. **SignalRGB
  does not hold the device exclusively.**

## Control experiment

A run of 90 seconds recorded:

| Collection | Reports | Meaning |
| --- | --- | --- |
| `0001/0006` | 0 | refused at open, as expected |
| `000C/0001` | 120 | mute and volume wheel, proves reading works |
| `FFC0/0002` | 55 | the G keys |
| `FFC2/0003` | 0 | unused by this device |
| `FFC2/0004` | 0 | command channel, we read nothing and wrote nothing |

The consumer control traffic matters. Without it, zero reports on the vendor
pipes would have been ambiguous between "the keyboard is quiet" and "our read
is broken". With 120 reports arriving on a separate pipe through the same code
path, the zeros are real measurements.

## Report format

`FFC0/0002`, 64 bytes, report ID `0x03`:

```
byte  0        : 0x03   report ID, constant
bytes 1 to 15  : 0x00   always zero in every report observed
byte  16       : bitmask, G1 to G8
byte  17       : bitmask, G9 and G10 in bits 0 and 1
byte  18       : bitmask, G11 to G18
bytes 19 to 63 : 0x00   always zero in every report observed
```

It is a **state snapshot, not an event stream**. Byte 16 carried `0x03` while G1
and G2 were held together, and `0x07` for G1 plus G2 plus G3. All zero means
nothing held. So presses and releases come from diffing consecutive reports,
with no key repeat handling and no event ordering to get wrong.

## G key bit mapping

Captured one key at a time, each verified against its raw report.

| Key | Byte | Bit | Mask |
| --- | --- | --- | --- |
| G1 | 16 | 0 | `0x01` |
| G2 | 16 | 1 | `0x02` |
| G3 | 16 | 2 | `0x04` |
| G4 | 16 | 3 | `0x08` |
| G5 | 16 | 4 | `0x10` |
| G6 | 16 | 5 | `0x20` |
| G7 | 16 | 6 | `0x40` |
| G8 | 16 | 7 | `0x80` |
| G9 | 17 | 0 | `0x01` |
| G10 | 17 | 1 | `0x02` |
| G11 | 18 | 0 | `0x01` |
| G12 | 18 | 1 | `0x02` |
| G13 | 18 | 2 | `0x04` |
| G14 | 18 | 3 | `0x08` |
| G15 | 18 | 4 | `0x10` |
| G16 | 18 | 5 | `0x20` |
| G17 | 18 | 6 | `0x40` |
| G18 | 18 | 7 | `0x80` |

All 18 pairs are distinct. For Phase 1:

```rust
/// (byte offset in the 64 byte report, bit index) for G1 through G18.
/// Captured empirically on 2026-11-11; see docs/PHASE0-FINDINGS.md.
pub const G_KEY_BITS: [(usize, u8); 18] = [
    (16, 0), (16, 1), (16, 2), (16, 3), (16, 4), (16, 5), (16, 6), (16, 7),
    (17, 0), (17, 1),
    (18, 0), (18, 1), (18, 2), (18, 3), (18, 4), (18, 5), (18, 6), (18, 7),
];
```

### The gap at byte 17, bits 2 to 7

The mapping leaves six bits unaccounted for between G10 and G11. The K95 has
exactly six other software visible special keys: MR, M1, M2, M3, brightness and
Windows lock. So those six bits are **probably** the M and special keys.

This is a hypothesis. It was not tested, because the project scope is 18 G keys
only. It does not need testing to be safe, but it does carry one requirement:

> Phase 1 must mask reads to exactly the 18 bits in the table above. Treating
> any set bit in bytes 16 to 18 as a G key would make the M keys and the
> brightness key fire G key actions.

## Replication

The mapping was captured once, then independently reproduced in a second run
under different conditions (SignalRGB app closed). All 18 keys landed on exactly
the same byte and bit, in the same order. The table is replicated, not asserted.

## Timing and duplicate reports

Each press produces a pair of reports roughly 70 to 80 ms apart: one with the
bit set, one with the byte cleared. Two identical reports also arrived 1 ms
apart (`byte 18 = 0x80` at 30.564 s and 30.565 s).

Because the format is a state snapshot rather than an event stream, duplicates
need no special handling: diffing two identical states yields no transitions, so
a repeated report is naturally a no op. Phase 1 should rely on that property
rather than adding dedup logic.

## SignalRGB dependency: none

Resolved by measurement.

| Condition | G key reports | Control (volume wheel) | Conclusion |
| --- | --- | --- | --- |
| SignalRGB app running | 55 in 90 s | 120 | emits |
| App closed, service running | 37 in 60 s | not pressed | still emits |
| **Service stopped, no processes** | **37 in 60 s** | **74** | **still emits** |

For the third run `SignalRgb.Service` was stopped (`State: Stopped`,
`ProcessId: 0`) and no signal or rgb process was running. The G keys emitted
anyway, and the volume wheel delivered 74 reports through the same code path in
the same run, so the positive result is not an artefact of a half working read.

**This application has no dependency on SignalRGB, iCUE, or any other software.**
The gated write path from the original plan is not needed and should not be
built.

The service was stopped only for this test and was restarted afterwards
(`Running`, start type `Automatic`, unchanged).

### Why this is the expected behaviour

It also matches the ckb-next source. When ckb-next *deactivates* a keyboard it
explicitly rewrites every key to `IN_HID`, with the comment that otherwise "the
G keys will freeze the keyboard" on Linux. That only makes sense if the G keys
are on the Corsair vendor pipe by default, in hardware mode, with no software
having configured anything. Which is exactly what we observe: the G keys have no
default HID scancode, so the firmware routes them to the vendor pipe natively.

So the keyboard was never in a state somebody else put it in. This is simply how
the hardware behaves out of the box.

### Residual doubt

One narrow alternative remains: the firmware could be holding a software mode
set earlier in this boot by SignalRGB, and persisting it after the service
stopped. Unplugging and replugging the keyboard with SignalRGB stopped would
settle it.

Given the ckb-next corroboration above, this is unlikely, and the practical
consequence is nil either way: if some earlier-in-boot state were required,
SignalRGB is an auto-start service on this machine and would always provide it.
Not tested, deliberately.

## Reproducing

```
cargo build
target\debug\probe.exe list              enumerate collections and report sizes
target\debug\probe.exe listen 90         dump reports with a byte diff
target\debug\probe.exe map               guided G1 to G18 capture
```

The probe has no dependencies and declares every Win32 entry point by hand, so
the read only claim is checkable rather than asserted:

```
dumpbin /imports target\debug\probe.exe
```

Against HID.DLL this shows only read calls:

```
HidD_GetAttributes   HidD_GetHidGuid       HidD_GetPreparsedData
HidD_GetProductString  HidD_FreePreparsedData  HidP_GetCaps
```

No `WriteFile`, no `HidD_SetFeature`, no `HidD_SetOutputReport`, no
`DeviceIoControl`.

**One correction, since the audit shows it.** `NtWriteFile` *does* appear in the
import table. It belongs to the Rust standard library, which uses it for console
and file output, and is not reachable from any device handle this code opens.
So the accurate claim is **no HID write surface**, which the import table
proves, not "no write syscall exists in the process", which would be untrue of
any program that can print. Earlier drafts of this document and of the source
comments overstated it.

## Defects found and fixed during Phase 0

Recorded because each one would have produced a confidently wrong result.

1. **`stdin` moved into a thread inside a loop.** Would not compile past the
   first iteration, and would have deadlocked on the stdin lock.
2. **Read failure indistinguishable from silence.** `read_timeout` returned
   `None` both for a timeout and for a failed read, so a broken read path would
   have been reported as "the keyboard sends nothing", and we would have asked
   for authorisation to write the device based on it. Now returns a three way
   outcome carrying the Win32 error.
3. **Report ID counted as key data.** The diff included byte 0, the constant
   report ID `0x03`, so the first report of any run falsely showed bits 0 and 1
   rising. In the mapping run that would have recorded the report ID as G1's bit
   and corrupted the whole table. The diff now skips byte 0.
4. **Mapping countdown started before the operator was ready.** The first
   mapping attempt timed out all three of its keys and bailed, which looked
   exactly like "the G keys stopped working". Added a ready gate.
