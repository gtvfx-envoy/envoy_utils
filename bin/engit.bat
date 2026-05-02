@echo off
REM engit -- git and GitHub tooling for envoy bundle development.
REM Usage: engit tag --patch
REM        engit release
REM        engit search <query>

REM Prefer the pre-built standalone executable when available.
if exist "%~dp0..\dist\engit.exe" (
    "%~dp0..\dist\engit.exe" %*
    exit /b %errorlevel%
)

REM Fall back to running from source (development mode).
set "PYTHONPATH=%~dp0..\py;%PYTHONPATH%"
python -m engit %*
exit /b %errorlevel%
