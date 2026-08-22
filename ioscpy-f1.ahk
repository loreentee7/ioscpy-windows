#NoEnv
#SingleInstance, Force
; ------------------------------------------------------------------
; Press F1 to launch ioscpy (mirror + control the iPhone over USB).
; It runs run-ioscpy.cmd, which sets PATH and starts ioscpy.exe.
; If ioscpy is already running, F1 does nothing (no duplicate window).
; ------------------------------------------------------------------

F1::
    Process, Exist, ioscpy.exe
    if (ErrorLevel != 0)   ; ioscpy.exe already running -> skip
        return
    Run, C:\Users\ahmed\ioscpy\run-ioscpy.cmd,, Hide
return
