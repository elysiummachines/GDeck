# Changelog

Notable changes, newest first. Each version's section is what the GitHub
release for that tag says, because `release.yml` reads this file rather than
writing its own notes.

## v0.1.0

First release. Maps the 18 G keys on a Corsair K95 RGB (`1b1c:1b11`) to
applications, folders, notes and shell commands, with no Corsair software, no
service, no driver and no network.

### The keyboard

- Reads one vendor HID collection, where the G keys report as an 18 bit state
  bitmap. Decoding masks to exactly those 18 bits, so the M keys, the
  brightness key and the volume wheel cannot leak through as a G key press.
- **Read only, and checkable.** HID imports are six `Get` calls, with no
  `HidD_Set`, no `DeviceIoControl` and no Win32 `WriteFile`. The keyboard is
  never written to, so there is no device state to restore on exit and nothing
  to contend with SignalRGB over. `verify-binary.ps1` asserts this on every
  build.
- **No keyboard hook**, for G keys or anything else. Windows claims keyboard
  top level HID collections exclusively, so the app cannot read your typing
  even if it tried.

### The interface

- A 3 by 6 grid of the 18 keys, lighting as they are physically pressed.
- Editor with a native file picker, for launch, open, note and shell actions.
- Real icons pulled from each target with `SHGetFileInfo`.
- **Deck mode**: hides the editor and fires on tap, for an external touch
  display. F11 for fullscreen, Escape to leave.
- **Drag to reorder.** Drag a tile onto another and the two swap.
- **Security panel** showing every live access control entry resolved to
  names, with the raw descriptor, drift detection against policy, and a box
  for applying your own.
- Lives in the notification area. Closing the window hides it, so the keys
  keep working. Quit on the tray menu is the way out.
- **One instance.** A second launch tells the copy already running to show
  itself and exits, so clicking a pinned taskbar button does not accumulate
  keyboard readers.
- **Opt in autostart**, off by default, through the per user Run key.

### The config

- Hand written line based parser, so a Windows path needs no escaping and
  round trips verbatim.
- Protected DACLs on the store directory, config, seal and audit log, with
  inheritance severed and no group entries.
- DPAPI sealed baseline with line by line drift detection. A config changed
  outside the app disarms every key until you confirm the diff.
- Launch targets re-validated at press time. Relative paths, bare command
  names, UNC paths and URI schemes outside a small allowlist are refused.
- Append only audit log of what each key ran, with the owner entry withholding
  `FILE_WRITE_DATA` so entries cannot be rewritten in place.
- Shell actions off by default.

### Documentation

- `docs/THREAT-MODEL.md`: assets, actors, and which defences are real against
  whom, including the ones that are not.
- `docs/PHASE0-FINDINGS.md`: what the hardware probe established, and the four
  defects found in the probe itself before its results were trusted.
- `scripts/exit-matrix.ps1`: every way the app can stop and what each leaves
  behind.
