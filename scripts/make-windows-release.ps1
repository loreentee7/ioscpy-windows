# Assemble a download-and-run Windows release zip for ioscpy.
# Bundles the built exe + the MSYS2/UCRT64 runtime DLLs it links against
# (libstdc++-6, libgcc_s_seh-1, libwinpthread-1) + a portable launcher and
# a quick-start note. libimobiledevice tools (iproxy/idevice_id) remain a
# documented prerequisite (install via `scoop install libimobiledevice`).
#
# Usage:
#   cargo build --release --manifest-path host/Cargo.toml
#   powershell -ExecutionPolicy Bypass -File scripts/make-windows-release.ps1 -Version 0.1.0
param(
  [string]$Version = "dev",
  [string]$MsysBin = "C:\msys64\ucrt64\bin"
)
$ErrorActionPreference = "Stop"
$root  = Split-Path -Parent $PSScriptRoot
$exe   = Join-Path $root "host\target\release\ioscpy.exe"
$dlls  = @("libstdc++-6.dll", "libgcc_s_seh-1.dll", "libwinpthread-1.dll")

if (-not (Test-Path $exe)) {
  throw "ioscpy.exe not found. Build first: cargo build --release --manifest-path host/Cargo.toml"
}

$stage = Join-Path $env:TEMP "ioscpy-windows-stage"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item $exe $stage
foreach ($d in $dlls) {
  $src = Join-Path $MsysBin $d
  if (-not (Test-Path $src)) { throw "Runtime DLL missing: $src (set -MsysBin to your UCRT64 bin)" }
  Copy-Item $src $stage
}
if (Test-Path (Join-Path $root "ioscpy-f1.ahk")) { Copy-Item (Join-Path $root "ioscpy-f1.ahk") $stage }

# Portable launcher: DLLs sit beside the exe, so they resolve automatically.
@'
@echo off
REM ioscpy portable launcher. The runtime DLLs sit next to ioscpy.exe and load
REM automatically. You still need libimobiledevice tools (iproxy, idevice_id) on
REM PATH -- install with: scoop install libimobiledevice
"%~dp0ioscpy.exe" %*
'@ | Set-Content (Join-Path $stage "run-ioscpy.cmd") -Encoding ascii

@"
ioscpy for Windows -- quick start
=================================

Prerequisites (once):
  1) Apple Mobile Device Support: install iTunes from apple.com (NOT the
     Microsoft Store build), or the standalone AMDS package. iTunes must see
     your iPhone.
  2) libimobiledevice tools on PATH:  scoop install libimobiledevice
     Verify:  idevice_id -l   (prints your device UDID)
  3) iPhone side: install ioscpy from the Sileo/Zebra repo:
       https://lautarovculic.github.io/ioscpy-repo/
     then respring.

Run:
  - Double-click run-ioscpy.cmd, or open a terminal here and run:
      ioscpy.exe --list      (confirm the device is seen)
      ioscpy.exe             (open the mirror)
  - After connecting or respringing, give the phone ONE physical tap before
    driving it from the PC (iOS only trusts injected touches after a real one).

Controls:
  - Left-click = tap, drag = swipe.
  - Right-click = Back, middle-click = Home, 4th mouse button = App Switcher.
  - Ctrl+J/L/T/R = Home / Lock / App Switcher / Rotate. Esc = Back.

Windows-focused fork of github.com/lautarovculic/ioscpy (MIT). Version $Version.
"@ | Set-Content (Join-Path $stage "README.txt") -Encoding utf8

$out = Join-Path $root ("ioscpy-windows-{0}.zip" -f $Version)
Remove-Item $out -Force -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $out

Write-Output ("Release zip: {0}" -f $out)
Get-ChildItem $stage | Select-Object Name, @{N='KB';E={[math]::Round($_.Length/1KB,1)}} | Format-Table -AutoSize
