@echo off
REM engit -- git and GitHub tooling for envoy bundle development.
REM Usage: engit tag --patch
REM        engit release
REM        engit search <query>

REM Set PYTHONPATH to find the engit module
set "PYTHONPATH=%~dp0..\py;%PYTHONPATH%"

REM Execute the engit CLI module
python -m engit %*

REM Exit with the same code as the Python process
exit /b %errorlevel%
