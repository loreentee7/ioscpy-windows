@echo off
REM Launcher for ioscpy — sets PATH so libimobiledevice tools + runtime DLLs resolve,
REM then runs the built ioscpy.exe. Pass any flags through, e.g.:  run-ioscpy.cmd --list
setlocal
set "PATH=C:\msys64\ucrt64\bin;%PATH%"
"%~dp0host\target\release\ioscpy.exe" %*
