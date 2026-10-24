@echo off
rem Build gitstat.exe on this PC into the repo's build\ folder.
rem Usage: build.cmd [commit]   (default: HEAD; commit your changes first)
rem
rem   build\gitstat.exe                          the newest build
rem   build\builds\<version>-<commit>\gitstat.exe  the last 4 builds
rem   build\builds\<version>-<commit>\src\       the git worktree each one was built from
setlocal enabledelayedexpansion
cd /d "%~dp0"
set "REF=%~1"
if "%REF%"=="" set "REF=HEAD"

for /f %%c in ('git rev-parse --short "%REF%"') do set "SHA=%%c"
if "%SHA%"=="" exit /b 1
for /f "tokens=2 delims== " %%v in ('git show "%SHA%:Cargo.toml" ^| findstr /b /c:"version"') do if not defined VER set "VER=%%~v"
set "NAME=%VER%-%SHA%"
set "OUT=%~dp0build\builds\%NAME%"
echo Building gitstat %NAME%

if not exist "%OUT%\src" git worktree add --detach "%OUT%\src" %SHA% || exit /b 1
rem One shared target folder inside build\ keeps rebuilds incremental.
set "CARGO_TARGET_DIR=%~dp0build\target"
pushd "%OUT%\src"
cargo build --release --locked || (popd & exit /b 1)
popd
copy /y "%CARGO_TARGET_DIR%\release\gitstat.exe" "%OUT%\gitstat.exe" >nul || exit /b 1
copy /y "%OUT%\gitstat.exe" "%~dp0build\gitstat.exe" >nul || echo build\gitstat.exe is in use: close gitstat and copy it from %OUT%.

rem Keep the newest 4 build folders, removing older ones with their worktrees.
set /a N=0
for /f "delims=" %%d in ('dir /b /ad /o-d "%~dp0build\builds"') do (
  set /a N+=1
  if !N! gtr 4 (
    git worktree remove "%~dp0build\builds\%%d\src"
    rmdir /s /q "%~dp0build\builds\%%d"
  )
)
del /q "%~dp0build\debug-*" >nul 2>&1

echo Done: build\gitstat.exe  (%NAME%)
endlocal
