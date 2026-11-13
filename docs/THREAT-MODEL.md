# G Deck threat model

What this program can be made to do, by whom, and what actually stops it.

Every claim below is either a property of the code you can read, or something
measured on the built binary with the commands given at the end. Where a
defence does not work, it says so.

## The short version

G Deck is a list of things to run, attached to 18 keys. The config file is
therefore a list of commands that execute as you. Anyone who can write that
file can run code as you.

The honest conclusion of everything that follows is this: **G Deck does not
meaningfully widen the attack surface of this machine, because everything it
can do, code already running as your account could do directly and more
easily.** It needs no administrator rights, installs no service and no driver,
opens no socket, and loads nothing at boot except itself, and only if you ask
it to.

The access control work is not defending you against yourself or against an
administrator. It defends the config against *other* standard accounts on the
machine, against inherited permission accidents, and against offline or
cross-user edits. That is a narrower claim than "secure", and it is the true
one.

## What is worth protecting

| Asset | Why it matters |
| --- | --- |
| `config.ini` | It is a list of executables, arguments and shell commands. Write access to it is code execution as you. |
| `config.seal` | The DPAPI sealed copy used to detect edits made outside the app. |
| `notes/` | Whatever you put in quick notes. |
| `audit.log` | The record of what each key actually ran. Its value is that it cannot be rewritten to hide an entry. |
| Your keystrokes | Not an asset G Deck holds. It is listed because the obvious fear about a key mapper is that it reads everything you type, and the design makes that impossible rather than merely forbidden. See below. |

## Who is in the picture

| Actor | What they can already do, before G Deck exists |
| --- | --- |
| You | Everything in your own account. |
| Another standard account on this PC | Their own files. Not yours, unless permissions are loose. |
| A local administrator, or SYSTEM | Anything. Take ownership of any file, rewrite any descriptor, read any process. |
| Malware running as you | Everything you can do, including reading and writing your files and calling DPAPI as you. |
| A remote attacker | Nothing through G Deck. It has no network code. |
| Someone with physical access and your unlocked session | The same as you. |

The second column is the important one. A defence is only worth something if
it stops an actor from doing something they could not already do.

## Threats, and whether anything stops them

| Threat | Stopped? | By what, and what is left |
| --- | --- | --- |
| Another standard account on this PC edits your config to run their payload as you | **Yes** | The store directory and every file in it carry a protected DACL granting only you and SYSTEM. Inheritance is severed, so no parent folder ACE can flow in. They would have to take ownership first, which they cannot do without administrator rights, and which would visibly change the owner the Security panel shows. |
| A permission accident, for example the folder inheriting `BUILTIN\Users` from a parent | **Yes** | `D:PAI` severs inheritance, and the app actively reports well known over broad trustees as EXPOSED when it finds them. |
| Someone edits `config.ini` offline, or from another account, or with a text editor | **Detected, not prevented** | The DPAPI seal will not match, the app refuses to fire any key, and it shows a line by line diff of what changed. You have to confirm before it arms again. |
| Malware running **as you** rewrites the config | **No** | It holds your token. It can write the file, and it can call `CryptProtectData` as you to produce a matching seal, so it can also hide the edit. The seal is tamper evidence against the row above, not against this one. |
| An administrator or SYSTEM alters anything | **No** | They can take ownership and rewrite any descriptor. The optional `BUILTIN\Administrators` ACE is therefore convenience, not protection, and removing it locks nobody out. The app says this in its own Security panel rather than implying otherwise. |
| G Deck reads what you type | **Cannot happen** | Windows claims keyboard top level HID collections exclusively. Opening one returns ACCESS_DENIED, which was measured in Phase 0 rather than assumed. G Deck reads one vendor collection that carries an 18 bit state bitmap and nothing else. No keyboard hook is installed, for G keys or anything else, and the import table has no hook function in it. |
| G Deck sends anything anywhere | **Cannot happen** | There is no network code. The import table of the shipped binary contains no `ws2_32`, `wininet`, `winhttp`, `urlmon` or `dnsapi`. |
| G Deck reprograms or bricks the keyboard | **Cannot happen** | There is no HID write surface. The binary imports exactly six functions from `HID.DLL`, all of them `Get`, with no `HidD_Set*`, no `DeviceIoControl` and no Win32 `WriteFile`. The device is never written to, so there is no device state to restore and nothing to collide with SignalRGB over. |
| A key launches something other than what you set | **Mitigated** | Targets are validated again at press time, not only when the file is parsed. `CreateProcessW` is given the absolute path as `lpApplicationName`, so Windows never searches `PATH` and the unquoted path problem does not arise. Relative paths, bare command names, UNC paths and URI schemes outside a small allowlist are refused. |
| The audit log is rewritten to hide what ran | **Mitigated** | The owner ACE withholds `FILE_WRITE_DATA`, so existing bytes cannot be overwritten in place; append and delete are granted, the latter so the log can be rotated. An administrator can still remove the file entirely. |
| A crafted label or target forges an audit entry | **Yes** | Fields are sanitised before they are written, so a value containing tabs or newlines cannot fabricate a row. |
| A malformed config is partly applied | **No, it is refused** | Parsing is all or nothing. A config that fails validation is rejected at the line that caused it and nothing is armed. |

## What the autostart toggle changes

Turning it on writes one value under `HKEY_CURRENT_USER`, holding this
executable's path in quotes. The quoting matters: unquoted, a path containing
a space lets Windows try shorter prefixes first, so a file planted beside a
parent directory would run instead.

It does not give an attacker a new capability, because anything running as you
could write that key itself. What it does change is the **consequence** of the
config being tampered with: with autostart on, a payload written into the
config runs at every sign in rather than only when you next open G Deck. If
you are worried about that specific case, leave autostart off.

Machine wide autostart is not offered at all. It would need administrator
rights and would start G Deck for every account on the computer.

## Weaknesses that are known and accepted

These are real. They are accepted because in every case the actor who could
exploit them is already running as you, and could simply kill the process or
edit the file directly instead.

- **The tray's window can be driven by any process in your session.** It is a
  message only window with a fixed class name, so another program can find it
  and post the messages that mean "show yourself" or "shut down". This was
  demonstrated during testing, which is how the exit matrix drives it. The
  worst outcome is that G Deck quits.
- **The single instance mutex name is predictable.** A hostile process could
  create `Local\GDeck.SingleInstance.v1` first and stop G Deck from starting.
  Denial of service, by something that could equally kill the process.
- **Shell commands being off by default is a speed bump, not a boundary.** A
  launch action with arguments can already run anything, so enabling shell
  does not grant a capability that was previously unreachable. It exists so
  that the common case carries a visible warning and an explicit decision.
- **Icon extraction runs shell code on paths from the config.**
  `SHGetFileInfoW` consults the shell's handler for the target, so a config
  naming an unusual path involves that handler. Same actor, same conclusion.
- **A hard kill leaves the notification icon behind** until something hovers
  over it, because the removal never runs. True of every program.
- **`panic = "abort"`** means any panic anywhere, including on a background
  thread, ends the process. This is a deliberate trade: a key mapper that
  continues in an unknown state is worse than one that stops.

## Out of scope

- Protecting you from an administrator of this machine.
- Protecting you from code already running under your own account.
- Anything to do with lighting. SignalRGB owns the device's LEDs and G Deck
  never writes to the keyboard at all.
- M1, M2, M3, MR, the brightness key and the volume wheel. Only the 18 G keys
  are read, and decoding masks to exactly those 18 bits so the others cannot
  leak through as a key press.

## Re-verifying the claims yourself

The claims about the shipped binary are checkable in a few seconds. From the
workspace root, with the release build present:

```
dumpbin /imports target/release/gdeck-ui.exe
```

Look for what is **not** there: no `ws2_32`, `wininet`, `winhttp`, `urlmon` or
`dnsapi`, and under `HID.DLL` only `HidD_GetHidGuid`, `HidD_GetAttributes`,
`HidD_GetPreparsedData`, `HidD_FreePreparsedData`, `HidD_GetProductString` and
`HidP_GetCaps`. `NtWriteFile` does appear; it belongs to the Rust standard
library's file writing, not to the device.

The dependency claim is one command:

```
cargo tree -p gdeck
```

The core crate, which holds every security decision, the config parser, the
seal and the launcher, has no dependencies at all. Every Win32 call it makes
is declared by hand in `crates/gdeck/src/ffi.rs`, so the set of things it can
reach is a list you can read.

The access control claims can be checked against Windows rather than against
our own read back, which is how a false drift report was found during
development:

```
icacls %LOCALAPPDATA%\GDeck
```

And the exit behaviour:

```
pwsh -File scripts/exit-matrix.ps1
```
