@echo off
REM ============================================================
REM  Kaynnistaa muistikortin palautusohjelman jarjestelman-
REM  valvojan oikeuksin (raakalaitteen luku vaatii ne).
REM ============================================================
setlocal
set EXE=%~dp0target\release\muistikortti_palautus.exe

if not exist "%EXE%" (
  echo Suoritettavaa tiedostoa ei loytynyt:
  echo   %EXE%
  echo Rakenna ohjelma ensin komennolla: cargo build --release
  pause
  exit /b 1
)

net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Kohotetaan jarjestelmanvalvojan oikeudet...
  powershell -NoProfile -Command "Start-Process -FilePath '%EXE%' -Verb RunAs"
  exit /b 0
)

"%EXE%"
pause
endlocal