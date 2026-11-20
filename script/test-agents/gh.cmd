@echo off
rem The gh stand-in on Windows: the Python script of the same name beside this file.
python "%~dp0gh" %*
exit /b %ERRORLEVEL%
