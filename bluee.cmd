@echo off
setlocal
rem ---------------------------------------------------------------------------
rem  bluee launcher
rem
rem  Run with no arguments to open the desktop app. Anything else is passed
rem  straight through:  bluee chat  /  bluee reduce  /  bluee search "..."
rem
rem  cd's to its own folder first, because bluee resolves .venv, mcps/,
rem  persona/, skills/ and data/ relative to the project root.
rem ---------------------------------------------------------------------------
cd /d "%~dp0"

rem  Checks the short build path from .cargo\config.toml first, then cargo's
rem  own default - so this works whether or not you set target-dir.
set "EXE=D:\tgt\harness\release\harness.exe"
if not exist "%EXE%" set "EXE=D:\tgt\harness\debug\harness.exe"
if not exist "%EXE%" set "EXE=%~dp0target\release\harness.exe"
if not exist "%EXE%" set "EXE=%~dp0target\debug\harness.exe"

if not exist "%EXE%" (
  echo.
  echo   bluee isn't built yet. From this folder run:
  echo.
  echo       cargo build --release
  echo.
  echo   ^(if you set target-dir in .cargo\config.toml, output goes there^)
  echo.
  pause
  exit /b 1
)

if "%~1"=="" (
  "%EXE%" app
) else (
  "%EXE%" %*
)
