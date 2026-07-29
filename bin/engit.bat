@echo off
REM engit -- git and GitHub tooling for envoy bundle development.
REM Usage: engit tag --patch
REM        engit release
REM        engit search <query>


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

REM No Python fallback: engit is a native-only Rust binary (rust/engit-cli)
REM with no Python package -- unlike envoy, it never had a `pip install`able
REM library API to distribute. If none of the executables above were found,
REM build one with `cargo build --release` (from rust/) or `cargo build`.
echo engit executable not found. Build one with: cargo build --release (from rust/) 1>&2
exit /b 1
