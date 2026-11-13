<#
.SYNOPSIS
    Writes the tag's version into the workspace manifest.

.DESCRIPTION
    Exactly one place holds the version: `version` under [workspace.package]
    in the root Cargo.toml. All three crates inherit it with
    `version.workspace = true`, so a tagged build stamps one line and every
    binary carries the number.

    The pattern must match exactly once. If it matches zero or several times,
    something about the manifest has drifted, and stamping the wrong line is
    worse than failing the build.

    Cargo.lock records the workspace crates' versions too, so it is refreshed
    afterwards rather than left for the build to rewrite underneath itself.
#>
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$Manifest = 'Cargo.toml'
)

$ErrorActionPreference = 'Stop'

$version = $Tag -replace '^v', ''
if ($version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Tag '$Tag' is not a version like v1.2.3"
}

$text = Get-Content $Manifest -Raw

# Anchored to the start of a line so it cannot match a dependency's version,
# which is always indented or inline inside a table.
$pattern = '(?m)^(version\s*=\s*")[^"]*(")'
$hits = [regex]::Matches($text, $pattern).Count
if ($hits -ne 1) {
    throw "Expected exactly one top level version line in $Manifest, found $hits. The manifest has drifted from what this script expects."
}

$text = [regex]::Replace($text, $pattern, "`${1}$version`${2}")
Set-Content -Path $Manifest -Value $text -NoNewline
Write-Host "Stamped $Manifest to $version"

# Resolving rewrites Cargo.lock with the new workspace versions, so the build
# does not have to do it underneath itself.
#
# --no-deps matters. A full resolve walks every target platform, including
# ones this project never builds for, and then fails offline because a crate
# like android-activity is not in the local cache. Only the workspace members
# changed, so there is nothing to look up.
& cargo metadata --format-version 1 --no-deps --offline *> $null
if ($LASTEXITCODE -ne 0) {
    throw 'Could not refresh Cargo.lock after stamping the version'
}
Write-Host 'Refreshed Cargo.lock'

Select-String -Path $Manifest -Pattern '^version' |
    ForEach-Object { Write-Host "  $($_.Line.Trim())" }
