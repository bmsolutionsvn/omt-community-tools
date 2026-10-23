# End-to-end proof for Desktop Capture: send the primary screen as OMT-HX and
# receive one frame. Starts `omt-desktop-capture --selftest` in the background,
# waits for the source to appear in OMT discovery, receives with
# `omthx-recv --dump`, and fails when the BMP is missing or every pixel is zero.

[CmdletBinding()]
param(
    # OMT-HX checkout, or its build\release folder, that holds omthx-recv.exe.
    [string]$OmthxRoot = '',
    # Seconds the sender runs. The receiver takes the first 5 of them.
    [int]$Seconds = 15
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
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
        $names = @(& $recv --list 2>$null)
        $found = @($names | Where-Object { $_.Trim() -ieq $sourceName }).Count -gt 0
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
if ($bytes.Length -lt 54 -or $bytes[0] -ne 0x42 -or $bytes[1] -ne 0x4D) {
    throw "frame.bmp is not a BMP: $frame"
}
$dataOffset = [BitConverter]::ToInt32($bytes, 10)
$width = [BitConverter]::ToInt32($bytes, 18)
$height = [Math]::Abs([BitConverter]::ToInt32($bytes, 22))
$bpp = [BitConverter]::ToInt16($bytes, 28)
if ($dataOffset -lt 54 -or $dataOffset -ge $bytes.Length -or $width -le 0 -or $height -le 0) {
    throw "frame.bmp header is not usable: offset $dataOffset, ${width}x${height}, ${bpp} bpp"
}

# ISO-8859-1 maps bytes 1:1 to chars, so a regex scan beats a per-byte loop.
$pixels = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes, $dataOffset, $bytes.Length - $dataOffset)
if (-not [regex]::IsMatch($pixels, '[^\x00]')) {
    throw "frame.bmp is all black (${width}x${height}, ${bpp} bpp)"
}

Write-Host "PASS: received ${width}x${height}, ${bpp} bpp, non-black ($($bytes.Length) bytes)"