<#
.SYNOPSIS
    Prints one version's section of CHANGELOG.md.

.DESCRIPTION
    The release notes and the changelog are the same text by construction,
    rather than two descriptions of one release that drift apart.

    Sections are second level headings whose text begins with the version,
    with or without a leading v:

        ## v0.1.0 - 2026-11-13

    A tag with no section fails, which is deliberate. Publishing a release
    with empty notes is worse than not publishing it.
#>
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$Path = 'CHANGELOG.md'
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $Path)) {
    throw "$Path does not exist"
}

$version = $Tag -replace '^v', ''
$lines = Get-Content $Path

# Match "## v0.1.0", "## 0.1.0", and anything following such as a date, but
# not "## 0.1.0-rc1" or "## 0.1.10" when looking for 0.1.1.
$heading = '^##\s+v?' + [regex]::Escape($version) + '(\s|$)'

$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match $heading) { $start = $i; break }
}
if ($start -lt 0) {
    throw "No section for $Tag in $Path. Add a '## $Tag' heading before tagging."
}

$end = $lines.Count
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^##\s') { $end = $i; break }
}

# The heading itself is dropped: the GitHub release already carries the tag
# as its title, so repeating it reads as a mistake.
$body = $lines[($start + 1)..($end - 1)]
($body -join "`n").Trim()
