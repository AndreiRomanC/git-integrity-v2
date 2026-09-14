@echo off
set "APP=%~dp0dist\windows\GitDrillDown.exe"
if exist "%APP%" goto launch
set "APP=%~dp0dist\windows\git-integrity.exe"
if not exist "%APP%" goto missing
echo Starting the previous Windows build. Run build-windows.ps1 to create GitDrillDown.exe.
:launch
start "Git DrillDown" "%APP%"
exit /b 0
:missing
echo Windows executable not found. Build it on Windows with build-windows.ps1.
pause
exit /b 1
