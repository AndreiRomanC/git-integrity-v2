@echo off
set "APP=%~dp0dist\windows\git-integrity.exe"
if not exist "%APP%" (
  echo Windows executable not found: %APP%
  echo Build it on Windows with build-windows.ps1.
  pause
  exit /b 1
)
start "Git Integrity" "%APP%"
