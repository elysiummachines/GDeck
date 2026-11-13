# G Deck exit matrix.
#
# Every way the app can stop, and what each one leaves behind. The point is
# not that it exits, but that it leaves nothing stuck: no notification icon
# with no process behind it, no single instance mutex still held so the next
# launch hands over to nothing, and no lock on the config so the file cannot
# be read or rewritten.
#
# Run it with nothing else of G Deck's running:
#   pwsh -File scripts/exit-matrix.ps1
#
# It starts and stops the app several times. It never signs you out: the
# session end case is posted to the window, which exercises the same path
# without touching the real session.

$ErrorActionPreference = 'Stop'

Add-Type -Namespace X -Name N -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll", CharSet=CharSet.Unicode)]
public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, IntPtr window);
[DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern IntPtr OpenMutexW(uint access, bool inherit, string name);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
[DllImport("user32.dll")]
public static extern IntPtr SendMessageTimeoutW(IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
[DllImport("user32.dll", CharSet=CharSet.Unicode)]
public static extern bool ShutdownBlockReasonQuery(IntPtr h, System.Text.StringBuilder buf, ref uint size);
'@

$exe       = Join-Path $PSScriptRoot '..\target\release\gdeck-ui.exe' | Resolve-Path
$store     = Join-Path $env:LOCALAPPDATA 'GDeck'
$config    = Join-Path $store 'config.ini'
$seal      = Join-Path $store 'config.seal'
$mutexName = 'Local\GDeck.SingleInstance.v1'

$WM_CLOSE          = 0x0010
$WM_COMMAND        = 0x0111
$WM_QUERYENDSESSION= 0x0011
$WM_ENDSESSION     = 0x0016
$ID_QUIT           = 2
$HWND_MESSAGE      = [IntPtr](-3)
$SYNCHRONIZE       = 0x00100000

function Tray-Window {
    [X.N]::FindWindowExW($HWND_MESSAGE, [IntPtr]::Zero, 'GDeckTrayWindow', [IntPtr]::Zero)
}

function Mutex-Held {
    $h = [X.N]::OpenMutexW($SYNCHRONIZE, $false, $mutexName)
    if ($h -ne [IntPtr]::Zero) { [void][X.N]::CloseHandle($h); return $true }
    return $false
}

# A lock is only visible by trying to take one that conflicts with it.
function Config-Locked {
    try {
        $s = [IO.File]::Open($config, 'Open', 'ReadWrite', 'None')
        $s.Close()
        return $false
    } catch { return $true }
}

function Fingerprint {
    $c = Get-Item $config; $s = Get-Item $seal
    "{0}:{1:o}|{2}:{3:o}" -f $c.Length, $c.LastWriteTimeUtc, $s.Length, $s.LastWriteTimeUtc
}

function Start-Deck {
    $p = Start-Process $exe -PassThru
    Start-Sleep -Seconds 7
    if ($p.HasExited) { throw "the app exited while starting, code $($p.ExitCode)" }
    $p
}

# Everything a terminal case has to satisfy, checked the same way every time.
function Check-Clean([string]$name, $proc, $expectExitCode) {
    Start-Sleep -Seconds 3
    $proc.Refresh()
    [pscustomobject]@{
        Case          = $name
        Exited        = $proc.HasExited
        ExitCode      = if ($proc.HasExited) { $proc.ExitCode } else { $null }
        ExitAsExpected= if ($null -eq $expectExitCode) { $proc.HasExited } else { $proc.HasExited -and $proc.ExitCode -eq $expectExitCode }
        TrayWindow    = if ((Tray-Window) -ne [IntPtr]::Zero) { 'still there' } else { 'gone' }
        MutexHeld     = Mutex-Held
        ConfigLocked  = Config-Locked
        ConfigChanged = (Fingerprint) -ne $script:baseline
    }
}

if (Get-Process -Name gdeck-ui -ErrorAction SilentlyContinue) {
    Write-Host 'ABORT: G Deck is already running. Quit it first.' -ForegroundColor Red
    exit 1
}

$script:baseline = Fingerprint
$results = @()

# --- 1 and 2: the close button hides rather than exits, and does so idempotently.
$p = Start-Deck
$win = $p.MainWindowHandle
[void][X.N]::PostMessageW($win, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
Start-Sleep -Seconds 3
$p.Refresh()
$results += [pscustomobject]@{
    Case = 'close button, window visible'; Exited = $p.HasExited; ExitCode = $null
    ExitAsExpected = (-not $p.HasExited) -and (-not [X.N]::IsWindowVisible($win))
    TrayWindow = if ((Tray-Window) -ne [IntPtr]::Zero) { 'still there' } else { 'gone' }
    MutexHeld = Mutex-Held; ConfigLocked = Config-Locked
    ConfigChanged = (Fingerprint) -ne $script:baseline
}

[void][X.N]::PostMessageW($win, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
Start-Sleep -Seconds 3
$p.Refresh()
$results += [pscustomobject]@{
    Case = 'close button again, already hidden'; Exited = $p.HasExited; ExitCode = $null
    ExitAsExpected = (-not $p.HasExited) -and (-not [X.N]::IsWindowVisible($win))
    TrayWindow = if ((Tray-Window) -ne [IntPtr]::Zero) { 'still there' } else { 'gone' }
    MutexHeld = Mutex-Held; ConfigLocked = Config-Locked
    ConfigChanged = (Fingerprint) -ne $script:baseline
}

# --- 3: quit from the tray menu while the window is hidden.
$tray = Tray-Window
[void][X.N]::PostMessageW($tray, $WM_COMMAND, [IntPtr]$ID_QUIT, [IntPtr]::Zero)
$results += Check-Clean 'tray Quit, window hidden' $p 0

# --- 4: quit from the tray menu with the window on screen.
$p = Start-Deck
$tray = Tray-Window
[void][X.N]::PostMessageW($tray, $WM_COMMAND, [IntPtr]$ID_QUIT, [IntPtr]::Zero)
$results += Check-Clean 'tray Quit, window visible' $p 0

# --- 5: Windows signing out.
#
# The app's job here is not to exit by itself. It is to answer "yes, go
# ahead" so sign out is not held up, and to leave nothing stuck when Windows
# then terminates it, which is exactly what the hard kill row below measures.
#
# This has to be sent rather than posted, because the answer is the whole
# point and a posted message has no answer to read. The first version of this
# script posted it and then recorded a failure when the app did not exit on
# its own, which was the test being wrong rather than the app.
#
# The real session is untouched: this asks one window a question.
$p = Start-Deck
$win = $p.MainWindowHandle
$answer = [IntPtr]::Zero
$SMTO_ABORTIFHUNG = 2
$replied = [X.N]::SendMessageTimeoutW(
    $win, $WM_QUERYENDSESSION, [IntPtr]::Zero, [IntPtr]::Zero,
    $SMTO_ABORTIFHUNG, 5000, [ref]$answer)
$reason = New-Object System.Text.StringBuilder 256
$reasonSize = [uint32]256
$blocks = [X.N]::ShutdownBlockReasonQuery($win, $reason, [ref]$reasonSize)
$results += [pscustomobject]@{
    Case           = 'sign out: answers, does not block'
    Exited         = $false
    ExitCode       = $null
    ExitAsExpected = $replied -and ($answer -ne [IntPtr]::Zero) -and (-not $blocks)
    TrayWindow     = if ((Tray-Window) -ne [IntPtr]::Zero) { 'still there' } else { 'gone' }
    MutexHeld      = Mutex-Held
    ConfigLocked   = Config-Locked
    ConfigChanged  = (Fingerprint) -ne $script:baseline
}
$p.Kill()
Start-Sleep -Seconds 2

# --- 6: killed outright, with no chance to clean up after itself.
$p = Start-Deck
$p.Kill()
$results += Check-Clean 'hard kill' $p $null

# --- Finally: the next launch must become the primary instance, which it can
#     only do if the mutex was genuinely released by every path above.
$p = Start-Deck
$primary = -not $p.HasExited
$tray = Tray-Window
[void][X.N]::PostMessageW($tray, $WM_COMMAND, [IntPtr]$ID_QUIT, [IntPtr]::Zero)
Start-Sleep -Seconds 4

$results | Format-Table -AutoSize
"a fresh launch became the primary instance: $primary"
"config and seal untouched throughout      : $((Fingerprint) -eq $script:baseline)"
if (Get-Process -Name gdeck-ui -ErrorAction SilentlyContinue) {
    Write-Host 'WARNING: something of G Deck is still running' -ForegroundColor Yellow
} else {
    'nothing left running'
}
