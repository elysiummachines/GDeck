<#
.SYNOPSIS
    Asserts, on the built binaries, the claims this project makes about them.

.DESCRIPTION
    G Deck's case rests on properties anyone can check rather than on
    promises. This script checks them, so a change that quietly breaks one
    fails the build instead of reaching a release.

    What it asserts:

      1. No networking library is imported. Not ws2_32, not winhttp, not
         wininet, not anything else that could open a socket or resolve a
         name. The app has no update check and no telemetry, and this is what
         makes that checkable rather than promised.

      2. Every HID import is a read. HidD_Get*, HidP_Get* and the one
         Free call that releases what a Get returned. Any HidD_Set*,
         HidP_Set* or flush fails the build. The keyboard is never written
         to, which is why there is no device state to restore and nothing to
         contend with SignalRGB over.

      3. No DeviceIoControl, which is the other way to reach a device.

      4. No Win32 WriteFile. NtWriteFile is expected and allowed: it belongs
         to the Rust standard library's file and console writing, not to the
         device. The distinction is the point, so it is asserted rather than
         waved at.

      5. The core crate still has no dependencies at all.

    It does not assert an exact binary size. That moves with the toolchain,
    and asserting it would produce failures that mean nothing.
#>

$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$failures = @()

function Fail([string]$message) {
    $script:failures += $message
    Write-Host "  FAIL  $message" -ForegroundColor Red
}

function Pass([string]$message) {
    Write-Host "  ok    $message" -ForegroundColor Green
}

# dumpbin ships with the MSVC build tools but is not on PATH. vswhere knows
# where the toolchain is; the recursive search is a fallback for layouts it
# does not report.
function Find-Dumpbin {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $install = & $vswhere -latest -products * -property installationPath 2>$null
        if ($install) {
            $hit = Get-ChildItem -Path (Join-Path $install 'VC\Tools\MSVC') -Filter dumpbin.exe -Recurse -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -like '*Hostx64\x64*' } |
                Select-Object -First 1
            if ($hit) { return $hit.FullName }
        }
    }
    foreach ($base in @("${env:ProgramFiles}\Microsoft Visual Studio", "${env:ProgramFiles(x86)}\Microsoft Visual Studio")) {
        if (Test-Path $base) {
            $hit = Get-ChildItem -Path $base -Filter dumpbin.exe -Recurse -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -like '*Hostx64\x64*' } |
                Select-Object -First 1
            if ($hit) { return $hit.FullName }
        }
    }
    return $null
}

$dumpbin = Find-Dumpbin
if (-not $dumpbin) {
    Write-Host 'Could not find dumpbin.exe. The MSVC build tools are required.' -ForegroundColor Red
    exit 1
}
Write-Host "dumpbin: $dumpbin"
Write-Host ''

$networking = @(
    'ws2_32', 'wsock32', 'mswsock', 'wininet', 'winhttp',
    'urlmon', 'dnsapi', 'iphlpapi', 'netapi32', 'httpapi', 'webservices'
)

$binaries = @(
    @{ Name = 'gdeck-ui.exe'; Path = Join-Path $root 'target\release\gdeck-ui.exe'; Hid = $true }
    @{ Name = 'gdeck.exe';    Path = Join-Path $root 'target\release\gdeck.exe';    Hid = $true }
)

foreach ($binary in $binaries) {
    Write-Host "=== $($binary.Name)"

    if (-not (Test-Path $binary.Path)) {
        Fail "$($binary.Name) was not built"
        continue
    }

    $size = (Get-Item $binary.Path).Length
    Write-Host ("  size  {0:N0} bytes ({1:N2} MB)" -f $size, ($size / 1MB))

    $imports = & $dumpbin /imports $binary.Path 2>$null | Out-String

    # 1. Networking.
    $found = @()
    foreach ($lib in $networking) {
        if ($imports -match "(?im)^\s*$([regex]::Escape($lib))\.dll") { $found += $lib }
    }
    if ($found.Count -gt 0) {
        Fail "networking libraries imported: $($found -join ', ')"
    } else {
        Pass 'no networking library imported'
    }

    if ($binary.Hid) {
        # 2. Every HID import must be a read.
        $hid = [regex]::Matches($imports, 'Hid[DP]_[A-Za-z0-9_]+') |
            ForEach-Object { $_.Value } | Sort-Object -Unique
        if ($hid.Count -eq 0) {
            Fail 'no HID imports at all, which is not what this binary should look like'
        } else {
            $writes = $hid | Where-Object { $_ -notmatch '^Hid[DP]_Get' -and $_ -ne 'HidD_FreePreparsedData' }
            if ($writes) {
                Fail "HID calls that are not reads: $($writes -join ', ')"
            } else {
                Pass "HID imports are all reads ($($hid.Count)): $($hid -join ', ')"
            }
        }
    }

    # 3. The other route to a device.
    if ($imports -match '\bDeviceIoControl\b') {
        Fail 'DeviceIoControl is imported'
    } else {
        Pass 'no DeviceIoControl'
    }

    # 4. Win32 WriteFile, as distinct from the standard library's NtWriteFile.
    if ($imports -match '(?m)^\s+[0-9A-F]+\s+WriteFile\s*$') {
        Fail 'Win32 WriteFile is imported'
    } else {
        Pass 'no Win32 WriteFile (NtWriteFile from the standard library is expected)'
    }

    Write-Host ''
}

# 5. The core crate's dependency count.
Write-Host '=== dependencies'
Push-Location $root
try {
    $tree = & cargo tree -p gdeck --prefix none 2>$null
    $deps = @($tree | Where-Object { $_ -match '\S' } | Where-Object { $_ -notmatch '^gdeck v' })
    if ($deps.Count -eq 0) {
        Pass 'the gdeck core crate has no dependencies'
    } else {
        Fail "the gdeck core crate gained $($deps.Count) dependencies: $($deps -join ', ')"
    }
} finally {
    Pop-Location
}

Write-Host ''
if ($failures.Count -gt 0) {
    Write-Host "$($failures.Count) check(s) failed:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
Write-Host 'All binary checks passed.' -ForegroundColor Green
