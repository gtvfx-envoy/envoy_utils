@echo off
REM engit -- git and GitHub tooling for envoy bundle development.
REM Usage: engit tag --patch
REM        engit release
REM        engit search <query>

REM Prefer the pre-built standalone executable when available (production /
REM published bundle layout -- see .github/workflows/build-release.yml).
if exist "%~dp0..\dist\engit.exe" (
    "%~dp0..\dist\engit.exe" %*
    exit /b %errorlevel%
)

REM Local dev build: native Rust binary built via `cargo build --release`
REM (or the debug profile) from rust/engit-cli, before a dist/ copy exists.
if exist "%~dp0..\rust\target\release\engit.exe" (
    "%~dp0..\rust\target\release\engit.exe" %*
    exit /b %errorlevel%
)
if exist "%~dp0..\rust\target\debug\engit.exe" (
    "%~dp0..\rust\target\debug\engit.exe" %*
    exit /b %errorlevel%
)

REM Fall back to running from source (development mode, pure Python).
set "PYTHONPATH=%~dp0..\py;%PYTHONPATH%"
python -m engit %*
exit /b %errorlevel%
