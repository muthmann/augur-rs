@echo off
rem Launcher for the portable AugurRS archive.
rem
rem Starts the GUI from wherever this folder was unpacked, and puts the folder
rem on PATH for the lifetime of the process so `augur` is callable from any
rem console this launcher opens. Nothing is written to the registry.

setlocal
set "AUGUR_HOME=%~dp0"
set "PATH=%AUGUR_HOME%;%PATH%"

if "%~1"=="cli" (
    shift
    "%AUGUR_HOME%augur.exe" %*
) else (
    start "" "%AUGUR_HOME%AugurRS.exe" %*
)
endlocal
