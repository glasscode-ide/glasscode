@echo off
rem The codex stand-in on Windows: the Python script of the same name beside this file.
python "%~dp0codex" %*
exit /b %ERRORLEVEL%
