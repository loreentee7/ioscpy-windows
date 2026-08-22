# Assemble a self-contained, download-and-run Windows release zip for ioscpy.
#
# Bundles:
#   - ioscpy.exe (built by cargo)
#   - the libimobiledevice CLI tools ioscpy shells out to: iproxy, idevice_id,
#     ideviceinfo
#   - the full transitive MSYS2/UCRT64 DLL closure for all of the above
#     (computed by scripts/dll-closure.py, not hardcoded)
#   - a portable launcher + quick-start
#
# The ONLY remaining prerequisite for users is Apple Mobile Device Support
# (the USB driver + usbmuxd service), which is a system install and cannot be
# bundled. openh264 is statically linked into ioscpy.exe; the UCRT
# (api-ms-win-crt-*) ships with Windows 10/11.
#
# Usage:
#   cargo build --release --manifest-path host/Cargo.toml
#   powershell -ExecutionPolicy Bypass -File scripts/make-windows-release.ps1 -Version 0.1.1
param(
  [string]$Version = "dev",
  [string]$MsysBin = "C:\msys64\ucrt64\bin"
)
$ErrorActionPreference = "Stop"
$root  = Split-Path -Parent $PSScriptRoot
$exe   = Join-Path $root "host\target\release\ioscpy.exe"
$tools = @("iproxy.exe", "idevice_id.exe", "ideviceinfo.exe")

if (-not (Test-Path $exe)) {
  throw "ioscpy.exe not found. Build first: cargo build --release --manifest-path host/Cargo.toml"
}
foreach ($t in $tools) {
  if (-not (Test-Path (Join-Path $MsysBin $t))) { throw "Tool missing: $t in $MsysBin" }
}

$stage = Join-Path $env:TEMP "ioscpy-windows-stage"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

# exe + tools
Copy-Item $exe $stage
foreach ($t in $tools) { Copy-Item (Join-Path $MsysBin $t) $stage }

# transitive DLL closure for exe + all tools
$rootBins = @($exe) + ($tools | ForEach-Object { Join-Path $MsysBin $_ })
$closure  = & python (Join-Path $PSScriptRoot "dll-closure.py") $MsysBin @rootBins
$missing  = $closure | Where-Object { $_ -like "MISSING*" }
if ($missing) { throw "Unresolved dependencies:`n$($missing -join "`n")" }
$dllPaths = $closure | Where-Object { $_ -like "BUNDLE *" } | ForEach-Object { $_.Substring(7) }
foreach ($d in $dllPaths) { Copy-Item $d $stage }
Write-Output ("Bundled {0} DLLs + {1} tools." -f $dllPaths.Count, $tools.Count)

if (Test-Path (Join-Path $root "ioscpy-f1.ahk")) { Copy-Item (Join-Path $root "ioscpy-f1.ahk") $stage }

# Portable launcher: exe, tools and DLLs all sit together, so everything resolves.
@'
@echo off
REM ioscpy portable launcher. ioscpy.exe, the libimobiledevice tools, and every
REM runtime DLL sit in this folder, so they all resolve automatically. Prepend
REM this folder to PATH for the session so ioscpy finds idevice_id / iproxy.
set "PATH=%~dp0;%PATH%"
"%~dp0ioscpy.exe" %*
'@ | Set-Content (Join-Path $stage "run-ioscpy.cmd") -Encoding ascii

@"
ioscpy for Windows -- quick start
=================================

This build is self-contained: ioscpy.exe, the libimobiledevice tools
(iproxy/idevice_id/ideviceinfo), and all runtime DLLs are in this folder.

Prerequisite (once):
  * Apple Mobile Device Support -- install iTunes from apple.com (NOT the
    Microsoft Store build), or the standalone AMDS package. This provides the
    USB driver + usbmux service. iTunes must see your iPhone. (This is a system
    driver and cannot be bundled.)
  * iPhone side: install ioscpy from the Sileo/Zebra repo, then respring:
      https://lautarovculic.github.io/ioscpy-repo/

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

Bundled third-party tools: iproxy, idevice_id, ideviceinfo and the
libimobiledevice / libplist / libusbmuxd libraries are LGPL-2.1+
(https://libimobiledevice.org). OpenSSL (libcrypto/libssl) is Apache-2.0.
They are redistributed unmodified from MSYS2.

Windows-focused fork of github.com/lautarovculic/ioscpy (MIT). Version $Version.
"@ | Set-Content (Join-Path $stage "README.txt") -Encoding utf8

$out = Join-Path $root ("ioscpy-windows-{0}.zip" -f $Version)
Remove-Item $out -Force -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $out

Write-Output ("Release zip: {0}" -f $out)
Get-ChildItem $stage | Select-Object Name, @{N='KB';E={[math]::Round($_.Length/1KB,1)}} | Format-Table -AutoSize
