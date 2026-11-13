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

      6. The fonts in the binary are the three the README names, with the two
         the linker drops still dropped. Checked by looking for each font
         file's own header in the executable rather than by reading the
         dependency list, because the licences that travel with a release are
         decided by the bytes that are in it. An egui update that swaps a font
         or revives a dropped one fails here instead of quietly making the
         licence section wrong.

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

# 6. The fonts the binary actually carries.
Write-Host '=== embedded fonts'

# The README's claim, as data. $true means the bytes are expected to be in
# the executable, $false means the linker is expected to drop them.
$claimed = [ordered]@{
    'Ubuntu-Light.ttf'      = $true
    'Hack-Regular.ttf'      = $true
    'egui-icons.ttf'        = $true
    'NotoEmoji-Regular.ttf' = $false
    'emoji-icon-font.ttf'   = $false
    'Inter-Regular.ttf'     = $true
    'Inter-SemiBold.ttf'    = $true
}

$ui = Join-Path $root 'target\release\gdeck-ui.exe'
if (-not (Test-Path $ui)) {
    Fail 'gdeck-ui.exe was not built, so its fonts cannot be checked'
} else {
    # Asking cargo where the crate was unpacked, rather than guessing at a
    # registry path, so this keeps working when the version moves.
    Push-Location $root
    try {
        $meta = (& cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc) | ConvertFrom-Json
    } finally {
        Pop-Location
    }
    $epaint = $meta.packages | Where-Object { $_.name -eq 'epaint_default_fonts' } | Select-Object -First 1

    if (-not $epaint) {
        Fail 'epaint_default_fonts is not in the dependency graph, so the font claim cannot be checked'
    } else {
        $dirs = @(
            (Join-Path (Split-Path $epaint.manifest_path) 'fonts')
            (Join-Path $root 'crates\gdeck-ui\assets')
        )
        $files = @(Get-ChildItem -Path $dirs -Filter *.ttf -ErrorAction SilentlyContinue)
        if ($files.Count -eq 0) {
            Fail 'no font files were found to check against, so this check proved nothing'
        }

        # Code page 28591 maps every byte to the character of the same value,
        # so IndexOf below is a byte search wearing a string's clothes.
        $latin1 = [System.Text.Encoding]::GetEncoding(28591)
        $haystack = $latin1.GetString([System.IO.File]::ReadAllBytes($ui))

        foreach ($file in $files) {
            # The first 512 bytes are the font's table directory, which is
            # unique to it and survives into the executable verbatim.
            $bytes = [System.IO.File]::ReadAllBytes($file.FullName)
            $take = [Math]::Min(512, $bytes.Length)
            $needle = $latin1.GetString($bytes, 0, $take)
            $present = $haystack.IndexOf($needle, [System.StringComparison]::Ordinal) -ge 0

            if (-not $claimed.Contains($file.Name)) {
                $where = if ($present) { 'and it is in the binary' } else { 'though the linker drops it' }
                Fail "$($file.Name) ships with a dependency but the README does not account for it, $where"
                continue
            }

            if ($present -eq $claimed[$file.Name]) {
                $state = if ($present) { '{0,7:N0} bytes, in the binary' -f $bytes.Length } else { 'dropped by the linker, as claimed' }
                Pass "$($file.Name.PadRight(22)) $state"
            } elseif ($present) {
                Fail "$($file.Name) is in the binary but the README says the linker drops it. Its licence now travels with every release."
            } else {
                Fail "$($file.Name) is named in the README but is not in the binary. The licence section claims more than it ships."
            }
        }

        $gone = @($claimed.Keys | Where-Object { $_ -notin $files.Name })
        if ($gone.Count -gt 0) {
            Fail "the README names fonts that no dependency ships any more: $($gone -join ', ')"
        }
    }
}

Write-Host ''

Write-Host ''
if ($failures.Count -gt 0) {
    Write-Host "$($failures.Count) check(s) failed:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
Write-Host 'All binary checks passed.' -ForegroundColor Green
