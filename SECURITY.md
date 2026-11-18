# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| The latest [release](https://github.com/elysiummachines/GDeck/releases) | Yes |
| Anything older | No |

G Deck is at `0.y.z`. There are no maintenance branches and nothing is
backported. A fix ships in the next release, and a security fix ships on its
own rather than waiting for other work to be ready.

## Reporting a problem

Use the [private advisory form](https://github.com/elysiummachines/GDeck/security/advisories/new).
It stays private between you and the maintainer until an advisory is
published.

Do not open a public issue for a security problem. The issue forms are for
bugs that affect function.

What helps, roughly in order of usefulness:

1. Whether you are reporting something this project claims cannot happen
2. What an attacker gains, and which account they have to be running as
3. The steps, including the config file if it is part of the reproduction
4. The version from `build-info.txt` in the zip, or the commit if you built it

The first point matters more than it looks.
[docs/THREAT-MODEL.md](docs/THREAT-MODEL.md) makes several flat claims: G Deck
cannot read what you type, cannot send anything anywhere, and never writes to
the keyboard. If you can demonstrate otherwise, that is the most valuable
report this project can receive, and it will be treated that way.

## In scope

* A way for anyone who is not you to run code through G Deck.
* A way to reach the store directory or any file in it from another standard
  account on the same machine.
* A config edited outside the app that the DPAPI seal fails to detect.
* A key press launching something other than its configured target, whether
  through `PATH` search, an unquoted path, a relative path, a UNC path or a
  URI scheme.
* An audit log entry overwritten in place, or a row forged through a crafted
  label or target.
* Any network traffic, any write to the keyboard, or any read of keyboard
  input beyond the 18 bit G key bitmap.
* Autostart writing anything other than this executable's quoted path under
  `HKEY_CURRENT_USER`.

## Out of scope

These are not oversights. Each one is in the threat model with the reason it
is accepted, and the reason is always the same shape: the actor who could
exploit it can already do the same thing more directly.

* **Anything that needs administrator or SYSTEM rights.** They can take
  ownership of any file and rewrite any descriptor, so no access control in a
  user mode program stops them. The optional `BUILTIN\Administrators` ACE is
  convenience, not protection.
* **Anything that needs code already running under your own account.** It
  holds your token, it can write your files, and it can call DPAPI as you,
  which means it can produce a seal that matches its own edit.
* **Stopping G Deck from starting, or making it quit.** The single instance
  mutex name is predictable, and the tray's message window can be driven by
  any process in your session. Both are denial of service by something that
  could kill the process instead.
* **Shell commands being off by default.** A launch action with arguments can
  already run anything, so the toggle is a visible decision rather than a
  boundary.
* **Anything to do with lighting.** G Deck never writes to the device.
* **M1, M2, M3, MR, the brightness key and the volume wheel.** Decoding masks
  to exactly the 18 G key bits, so the others cannot leak through as a press.

## Testing

G Deck opens no socket, installs no service and needs no administrator
rights, so there is nothing of anyone else's to test against. Test on your own
machine, against your own install.

## The claims are enforced, not just written down

`.github/scripts/verify-binary.ps1` runs in the release job, before anything
is packaged, and asserts against the built binaries: no networking library
imported, every HID import a read, no `DeviceIoControl`, no Win32 `WriteFile`,
and a core crate with no dependencies at all. A change that quietly breaks one
of those fails the build instead of reaching a release.

Checking it yourself takes a few seconds. The threat model gives the commands,
and every release carries a `.sha256` beside the zip.

## What happens after you report

This is one person's project, not a company with a queue.

* You should have an acknowledgement within seven days.
* If the report holds up, you will be told what the fix is before it ships.
* You will be credited in the advisory and the changelog unless you would
  rather not be.
* Please give it ninety days before going public, or less if we agree on less.

If seven days pass in silence, assume the report went astray rather than that
it was ignored. Open a public issue saying only that you are waiting on a
private report, with no details in it, and it will get picked up.
