# Copy the Desktop Capture binary and its OMT-HX sidecars into
# dist\desktop-capture and zip it. The folder is self-contained: extract it and
# run omt-desktop-capture.exe from inside it.

[CmdletBinding()]
param(
    # OMT-HX checkout, or its build\release folder, that holds omthx.dll and the
    # FFmpeg DLLs.
    [string]$OmthxRoot = '',
    # Rust target triple, for a `cargo build --release --target ...` folder.
    [string]$Target = ''
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

$profileDir = if ($Target) { Join-Path $root "target\$Target\release" } else { Join-Path $root 'target\release' }
$exe = Join-Path $profileDir 'omt-desktop-capture.exe'
if (-not (Test-Path $exe)) {
    throw "omt-desktop-capture.exe not found at $exe. Build it first: cargo build --release -p omt-desktop-capture"
}

$sidecarDir = @((Join-Path $OmthxRoot 'build\release'), $OmthxRoot) |
    Where-Object { Test-Path (Join-Path $_ 'omthx.dll') } |
    Select-Object -First 1
if (-not $sidecarDir) {
    throw "omthx.dll not found under $OmthxRoot. Pass -OmthxRoot with the OMT-HX checkout (or set OMTHX_ROOT)."
}

$out = Join-Path $root 'dist\desktop-capture'
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $out | Out-Null

Copy-Item $exe $out
foreach ($name in 'omthx.dll', 'avcodec-63.dll', 'avutil-61.dll', 'swscale-10.dll', 'swresample-7.dll', 'libomt.dll', 'libvmx.dll') {
    $from = Join-Path $sidecarDir $name
    if (-not (Test-Path $from)) { throw "missing sidecar: $from" }
    Copy-Item $from $out
}
Copy-Item (Join-Path $root 'apps\desktop-capture\README.md') $out
Copy-Item (Join-Path $root 'LICENSE') $out

$zip = Join-Path $root 'dist\desktop-capture.zip'
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $out -DestinationPath $zip -Force

$mb = [Math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "packaged $zip (${mb} MB)"
