# End-to-end proof for Desktop Capture: send the primary screen as OMT-HX and
# receive one frame. Starts `omt-desktop-capture --selftest` in the background,
# waits for the source to appear in OMT discovery, receives with
# `omthx-recv --dump`, and fails when the BMP is missing, the wrong size, or all
# black. `-SelfCheck` runs the BMP checker against synthetic frames only.

[CmdletBinding()]
param(
    # OMT-HX checkout, or its build\release folder, that holds omthx-recv.exe.
    [string]$OmthxRoot = '',
    # Seconds the sender runs. The receiver takes the first 5 of them.
    [int]$Seconds = 20,
    # Check the black-frame detection with synthetic BMPs and exit.
    [switch]$SelfCheck
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# Parse a BMP header into its pixel layout. omthx-recv writes 32-bpp BGRA.
function Get-FrameInfo([byte[]]$bytes) {
    if ($bytes.Length -lt 54 -or $bytes[0] -ne 0x42 -or $bytes[1] -ne 0x4D) {
        throw 'not a BMP'
    }
    $info = [pscustomobject]@{
        DataOffset = [BitConverter]::ToInt32($bytes, 10)
        Width = [BitConverter]::ToInt32($bytes, 18)
        Height = [Math]::Abs([BitConverter]::ToInt32($bytes, 22))
        Bpp = [BitConverter]::ToInt16($bytes, 28)
    }
    if ($info.DataOffset -lt 54 -or $info.DataOffset -ge $bytes.Length -or
        $info.Width -le 0 -or $info.Height -le 0) {
        throw "BMP header is not usable: offset $($info.DataOffset), $($info.Width)x$($info.Height), $($info.Bpp) bpp"
    }
    if ($info.Bpp -ne 32) { throw "BMP is $($info.Bpp) bpp; expected 32-bpp BGRA" }
    return $info
}

# True when any pixel has a non-zero B, G or R byte. Alpha is skipped: the
# receiver stores 255 there on every pixel, so it cannot prove a picture exists.
function Test-FrameNonBlack([byte[]]$bytes, $info) {
    for ($i = $info.DataOffset; $i + 3 -lt $bytes.Length; $i += 4) {
        if ($bytes[$i] -ne 0 -or $bytes[$i + 1] -ne 0 -or $bytes[$i + 2] -ne 0) { return $true }
    }
    return $false
}

if ($SelfCheck) {
    # A fully black 64x48 BGRA frame, alpha 255, exactly what a blocked capture
    # looks like on screen.
    $width = 64
    $height = 48
    $black = New-Object byte[] (54 + $width * $height * 4)
    $black[0] = 0x42
    $black[1] = 0x4D
    [BitConverter]::GetBytes([int]$black.Length).CopyTo($black, 2)
    [BitConverter]::GetBytes([int]54).CopyTo($black, 10)
    [BitConverter]::GetBytes([int]40).CopyTo($black, 14)
    [BitConverter]::GetBytes([int]$width).CopyTo($black, 18)
    [BitConverter]::GetBytes([int](-$height)).CopyTo($black, 22)
    [BitConverter]::GetBytes([int16]1).CopyTo($black, 26)
    [BitConverter]::GetBytes([int16]32).CopyTo($black, 28)
    [BitConverter]::GetBytes([int]($width * $height * 4)).CopyTo($black, 34)
    for ($i = 57; $i -lt $black.Length; $i += 4) { $black[$i] = 255 }

    $info = Get-FrameInfo $black
    if (Test-FrameNonBlack $black $info) {
        throw 'self-check: an all-black frame with alpha 255 was accepted'
    }
    $black[54 + (10 * $width + 10) * 4] = 1
    if (-not (Test-FrameNonBlack $black $info)) {
        throw 'self-check: a frame with one non-black pixel was rejected'
    }
    Write-Host 'SELF-CHECK PASS: black BGRA with alpha 255 rejected; one non-black pixel accepted'
    return
}

if (-not $OmthxRoot) {
    $OmthxRoot = if ($env:OMTHX_ROOT) {
        $env:OMTHX_ROOT
    } else {
        Join-Path (Split-Path -Parent $root) 'OMT-HX'
    }
}

$exe = Join-Path $root 'target\release\omt-desktop-capture.exe'
if (-not (Test-Path $exe)) {
    throw "omt-desktop-capture.exe not found at $exe. Build it first: cargo build --release -p omt-desktop-capture"
}

$sidecarDir = @((Join-Path $OmthxRoot 'build\release'), $OmthxRoot) |
    Where-Object { Test-Path (Join-Path $_ 'omthx-recv.exe') } |
    Select-Object -First 1
if (-not $sidecarDir) {
    throw "omthx-recv.exe not found under $OmthxRoot. Pass -OmthxRoot with the OMT-HX checkout (or set OMTHX_ROOT)."
}
$recv = Join-Path $sidecarDir 'omthx-recv.exe'

# The DLLs live beside the binary in the shipped folder; point the loader at the
# same build when the release folder has no sidecars of its own.
if (-not $env:OMTHX_LIBRARY -and -not (Test-Path (Join-Path (Split-Path -Parent $exe) 'omthx.dll'))) {
    $env:OMTHX_LIBRARY = Join-Path $sidecarDir 'omthx.dll'
}

$logDir = Join-Path $root 'target\desktop-capture-e2e'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$frame = Join-Path $logDir 'frame.bmp'
Remove-Item $frame -ErrorAction SilentlyContinue

$sourceName = "$env:COMPUTERNAME (Screen 1)"
$sender = Start-Process -FilePath $exe `
    -ArgumentList '--selftest', '--seconds', $Seconds `
    -RedirectStandardOutput (Join-Path $logDir 'sender.out.log') `
    -RedirectStandardError (Join-Path $logDir 'sender.err.log') `
    -PassThru
try {
    # Discovery needs a couple of seconds after the sender opens its socket.
    $deadline = (Get-Date).AddSeconds(12)
    $found = $false
    while (-not $found -and (Get-Date) -lt $deadline) {
        if ($sender.HasExited) {
            $tail = Get-Content (Join-Path $logDir 'sender.err.log') -Tail 10 -ErrorAction SilentlyContinue
            throw "sender stopped before discovery finished (code $($sender.ExitCode)): $($tail -join ' ')"
        }
        # Merge stderr so a discovery failure is reported, but do not let
        # PowerShell turn native stderr into a terminating error.
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $list = @(& $recv --list 2>&1 | ForEach-Object { "$_" })
        $listExit = $LASTEXITCODE
        $ErrorActionPreference = $previous
        if ($listExit -ne 0) {
            throw "omthx-recv --list exited with code ${listExit}: $($list -join ' ')"
        }
        $found = @($list | Where-Object { $_.Trim() -ieq $sourceName }).Count -gt 0
        if (-not $found) { Start-Sleep -Milliseconds 500 }
    }
    if (-not $found) { throw "source '$sourceName' did not appear in OMT discovery within 12 s" }

    & $recv --source $sourceName --seconds 5 --dump $frame
    if ($LASTEXITCODE -ne 0) { throw "omthx-recv exited with code $LASTEXITCODE" }
}
finally {
    if ($sender -and -not $sender.HasExited) { Stop-Process -Id $sender.Id -Force }
}

if (-not (Test-Path $frame)) { throw "frame.bmp missing: omthx-recv received no video" }

$bytes = [System.IO.File]::ReadAllBytes($frame)
try {
    $info = Get-FrameInfo $bytes
} catch {
    throw "frame.bmp is not a usable BMP: $($_.Exception.Message)"
}

$primary = Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | Select-Object -First 1
if ($primary -and $primary.CurrentHorizontalResolution -gt 0 -and $primary.CurrentVerticalResolution -gt 0) {
    if ($info.Width -ne $primary.CurrentHorizontalResolution -or $info.Height -ne $primary.CurrentVerticalResolution) {
        throw "frame is $($info.Width)x$($info.Height), primary display is $($primary.CurrentHorizontalResolution)x$($primary.CurrentVerticalResolution)"
    }
} else {
    Write-Warning 'primary display size unavailable; skipping the size check'
}

if (-not (Test-FrameNonBlack $bytes $info)) {
    throw "frame.bmp is all black ($($info.Width)x$($info.Height), 32 bpp, B/G/R only)"
}

Write-Host "PASS: received $($info.Width)x$($info.Height), 32 bpp, non-black ($($bytes.Length) bytes)"
