@echo off
REM engit -- git and GitHub tooling for envoy bundle development.
REM Usage: engit tag --patch
REM        engit release
REM        engit search <query>

@REM Test if %EDITOR% is set, if not set it to a default value
if not defined EDITOR (
    set "EDITOR=C:\Windows\notepad.exe"
)

REM Set PYTHONPATH to find the engit module
set "PYTHONPATH=%~dp0..\py;%PYTHONPATH%"

REM Execute the engit CLI module
python -m engit %*

REM Exit with the same code as the Python process
exit /b %errorlevel%
