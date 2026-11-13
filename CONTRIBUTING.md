# Contributing

G Deck's case rests on properties anyone can check rather than on promises.
Most of what follows is about keeping those properties checkable.

## Branches

`main` is the only long lived branch. Everything else is short lived and
named with a prefix and a kebab case description of the work:

| Prefix | For |
|---|---|
| `feat/` | A new capability |
| `fix/` | A defect |
| `docs/` | Documentation only |
| `chore/` | CI, build, dependencies, housekeeping |

```
feat/deck-profiles
fix/tray-ghost-icon
docs/threat-model-actors
chore/bump-egui-0.38
```

Open a pull request for every branch, draft is fine. CI runs on pull requests
to `main` and on pushes to `main`, so a branch with no pull request open is a
branch that nothing is checking.

Branches made by a Claude Code session arrive as `claude/<name>`. That prefix
is one account wide string rather than one per task, so it cannot say `feat`
or `fix`. Rename the branch or open the pull request from it as it stands;
nothing downstream reads the name.

## Commit messages

Imperative, sentence case, no full stop:

```
Assert the font claim on the binary, not on the dependency list
```

Then a blank line and a body saying why. The diff already says what.

Conventional Commits are deliberately not used. The changelog here is written
by hand, so a machine readable prefix on every subject line would buy nothing
and cost readability.

Commits on `main` and every release tag in this repository are signed. That
is a property of this repository's history, not a requirement placed on a
pull request from someone else.

## What has to pass

CI runs on `windows-latest`, and all of it has to be green:

| Check | Command |
|---|---|
| Formatting | `cargo fmt --all --check` |
| Lints, where warnings are errors | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Tests | `cargo test --workspace --locked` |
| The autostart test, which writes to the registry | `cargo test -p gdeck-ui --locked -- --ignored --nocapture` |
| Release build | `cargo build --workspace --release --locked` |
| The project's own claims, on the built binaries | `.\.github\scripts\verify-binary.ps1` |

Two of those need a word of warning before you run them locally.

The autostart test writes a value under
`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` and removes
it again. It refuses to run at all if a value is already there, so it can
never overwrite yours. If you have autostart turned on in G Deck, it will
print that it skipped and pass. On CI the same situation fails instead,
because a throwaway runner cannot legitimately have one.

`verify-binary.ps1` needs a release build to exist first, and it needs
`dumpbin` from the MSVC build tools, which it locates through `vswhere`.

## What that verification asserts

This is the check worth understanding before changing anything, because it is
what turns the README's claims into statements that fail the build when they
stop being true:

1. No networking library is imported. No socket, no name resolution, no
   update check, no telemetry.
2. Every HID import is a read. Any `HidD_Set`, `HidP_Set` or flush fails.
3. No `DeviceIoControl`.
4. No Win32 `WriteFile`. `NtWriteFile` from the standard library is expected
   and allowed, and the distinction is the point.
5. The `gdeck` core crate still has no dependencies at all.
6. The fonts in the binary are the ones the README names, with the two the
   linker drops still dropped, so an upstream change cannot quietly make the
   licence section wrong.

## Things this project will not take

Not because they are badly written, but because each one destroys a property
the project exists to have:

- Writing to the keyboard. The read only guarantee is the reason there is no
  device state to restore and nothing to contend with SignalRGB over.
- A network call of any kind, including an update check.
- A service, a driver, or a scheduled task. All three can be made to run as
  SYSTEM, which is far more than a key mapper needs.
- Writing to `HKEY_LOCAL_MACHINE`, or anywhere else machine wide.
- Reading keyboard input beyond the vendor collection the G keys report on.
- A dependency added to the `gdeck` core crate.

`docs/THREAT-MODEL.md` says which of these defences are real and against
whom, including the ones that are not. It is worth reading before arguing
with any of them.

## Releases

Tags come off `main` only, and the tag is what gets built:

```
git tag -s v0.2.0 -m "G Deck v0.2.0"
git push origin v0.2.0
```

Pushing the tag runs `release.yml`, which stamps the version into
`[workspace.package]`, builds, runs the binary verification, packages a zip
with a SHA-256 for each executable, and takes the release notes from this
repository's `CHANGELOG.md`.

So the changelog section has to exist before the tag is pushed.
`changelog-section.ps1` throws rather than publishing an empty release, and
`stamp-version.ps1` throws unless `Cargo.toml` has exactly one top level
version line, so a drifting manifest fails the release instead of being
stamped in the wrong place.
