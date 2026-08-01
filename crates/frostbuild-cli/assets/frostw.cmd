@echo off
rem frostw.cmd — run the frost release this repository declares.
rem
rem The Windows half of `frostw`. Same contract, same `.frost-version`, same
rem cache under %FROST_HOME%\versions: a workspace checked out on Windows and
rem on Linux runs the same frost. See the POSIX script for the reasoning.
rem
rem curl.exe, certutil and tar.exe all ship with Windows 10 1803 and later, so
rem the wrapper needs nothing installed to bootstrap itself.

setlocal EnableExtensions EnableDelayedExpansion

set "self=frostw"
set "here=%~dp0"
set "versionFile=%here%.frost-version"

if not exist "%versionFile%" (
    call :say "no %versionFile%. write one line naming the frost version this workspace requires, for example 0.9.0"
    exit /b 2
)

rem One line, no decoration. `eol=#` drops comment lines and `tokens=1` trims
rem the surrounding whitespace, so a pin may carry a note about why it is set.
set "version="
for /f "usebackq eol=# tokens=1 delims= " %%v in ("%versionFile%") do (
    if not defined version set "version=%%v"
)

if not defined version (
    call :say "%versionFile% names no version"
    exit /b 2
)

echo %version%|findstr /r /c:"^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$" >nul
if errorlevel 1 (
    call :say "%versionFile% names '%version%', which is not an X.Y.Z version"
    exit /b 2
)

if defined FROST_HOME (set "frostHome=%FROST_HOME%") else (set "frostHome=%USERPROFILE%\.frost")
set "installDir=%frostHome%\versions\%version%"
set "binary=%installDir%\frost.exe"

rem A matching frost already on PATH is the answer: no download, no cache, and
rem a developer who installed frost themselves is not second-guessed.
set "onPath="
for /f "tokens=2" %%v in ('frost --version 2^>nul') do if not defined onPath set "onPath=%%v"
if not "%onPath%"=="%version%" goto :cached
frost %*
exit /b %errorlevel%

:cached
if not exist "%binary%" goto :download
"%binary%" %*
exit /b %errorlevel%

:download
rem Only the triple the release workflow actually builds for Windows.
set "triple=x86_64-pc-windows-msvc"
set "archive=frostbuild-v%version%-%triple%.zip"
if defined FROSTW_RELEASE_BASE_URL (
    set "baseUrl=%FROSTW_RELEASE_BASE_URL%"
) else (
    set "baseUrl=https://github.com/hjosugi/frost-build/releases/download"
)
set "releaseUrl=%baseUrl%/v%version%"

where curl.exe >nul 2>&1
if errorlevel 1 (
    call :say "curl.exe is not available, so frost %version% cannot be downloaded"
    call :recovery
    exit /b 2
)
where tar.exe >nul 2>&1
if errorlevel 1 (
    call :say "tar.exe is not available, so the release archive cannot be unpacked"
    call :recovery
    exit /b 2
)

rem Staged inside the destination's own directory so the final move is a
rem rename on one volume: either the whole version appears, or none of it does.
set "staging=%frostHome%\versions\.frostw-%RANDOM%%RANDOM%"
mkdir "%staging%" 2>nul
if not exist "%staging%" (
    call :say "cannot create a staging directory under %frostHome%\versions"
    exit /b 2
)

call :say "downloading frost %version% (%triple%)"

curl.exe --fail --location --silent --show-error --output "%staging%\SHA256SUMS" "%releaseUrl%/SHA256SUMS"
if errorlevel 1 (
    call :say "cannot fetch %releaseUrl%/SHA256SUMS"
    call :say "either %version% is not a published release, or the network is unavailable"
    goto :abort
)

set "expected="
for /f "usebackq tokens=1,2" %%a in ("%staging%\SHA256SUMS") do (
    if /i "%%b"=="%archive%" set "expected=%%a"
    if /i "%%b"=="*%archive%" set "expected=%%a"
)
if not defined expected (
    call :say "release %version% publishes no %archive%"
    goto :abort
)

curl.exe --fail --location --silent --show-error --output "%staging%\%archive%" "%releaseUrl%/%archive%"
if errorlevel 1 (
    call :say "cannot fetch %releaseUrl%/%archive%"
    goto :abort
)

set "actual="
for /f "skip=1 tokens=*" %%h in ('certutil -hashfile "%staging%\%archive%" SHA256') do (
    if not defined actual set "actual=%%h"
)
rem Older certutil separates the hash into byte pairs.
set "actual=%actual: =%"

if /i not "%actual%"=="%expected%" (
    call :say "checksum mismatch for %archive%"
    call :say "  expected %expected%"
    call :say "  got      %actual%"
    call :say "the download was discarded and nothing was installed"
    goto :abort
)

rem Unpacked only after the checksum matched, and only into the staging
rem directory, so a rejected or truncated archive leaves no half-installed
rem version behind for the next run to trust.
tar.exe -xf "%staging%\%archive%" -C "%staging%"
if errorlevel 1 (
    call :say "cannot unpack %archive%"
    goto :abort
)

if not exist "%staging%\frostbuild-v%version%-%triple%\frost.exe" (
    call :say "%archive% does not contain frostbuild-v%version%-%triple%\frost.exe"
    goto :abort
)

rem A concurrent frostw may have installed the same version first. It unpacked
rem the same verified bytes, so losing that race is success.
move "%staging%\frostbuild-v%version%-%triple%" "%installDir%" >nul 2>&1
if not exist "%binary%" (
    call :say "cannot install frost %version% into %installDir%"
    goto :abort
)

rd /s /q "%staging%" 2>nul
"%binary%" %*
exit /b %errorlevel%

:abort
rd /s /q "%staging%" 2>nul
call :recovery
exit /b 2

rem What a reader can do by hand when the automatic path cannot finish.
rem Printed by every failure above, because "download failed" without this is
rem a dead end.
:recovery
call :say "to continue without this download, either:"
call :say "  * install frost %version% and put it on PATH, or"
call :say "  * unpack that release yourself so that %binary% exists"
exit /b 0

:say
echo %self%: %~1 1>&2
exit /b 0
