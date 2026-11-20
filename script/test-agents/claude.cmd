@echo off
rem The claude stand-in on Windows: the Python script of the same name beside this file.
python "%~dp0claude" %*
exit /b %ERRORLEVEL%
