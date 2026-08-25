@echo off
setlocal enabledelayedexpansion
title Baxter's Stereo Mix — Installer
echo ============================================================
echo   Baxter's Stereo Mix (BSM) — Installer
echo ============================================================
echo.

:: ── Locate project root (same folder as this .bat) ──
set "BSM_ROOT=%~dp0"
cd /d "%BSM_ROOT%"

:: ── 1. Check for Rust toolchain ──
echo [1/4] Checking Rust toolchain...
where rustc >nul 2>&1
if %errorlevel% neq 0 (
    echo   Rust not found. Installing via rustup...
    echo   Downloading rustup-init.exe ...
    powershell -Command "Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile '%TEMP%\rustup-init.exe'"
    if not exist "%TEMP%\rustup-init.exe" (
        echo   ERROR: Failed to download rustup-init.exe
        echo   Please install Rust manually from https://rustup.rs
        pause
        exit /b 1
    )
    "%TEMP%\rustup-init.exe" -y --default-toolchain stable
    if %errorlevel% neq 0 (
        echo   ERROR: Rust installation failed.
        pause
        exit /b 1
    )
    :: Refresh PATH for this session
    call "%USERPROFILE%\.cargo\env.bat" 2>nul
    set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
)

for /f "tokens=*" %%v in ('rustc --version 2^>nul') do set "RUSTVER=%%v"
echo   OK: !RUSTVER!
echo.

:: ── 2. Check for C compiler (needed by mp3lame-sys vendored build) ──
echo [2/4] Checking C compiler...
where cl >nul 2>&1
if %errorlevel% neq 0 (
    where gcc >nul 2>&1
    if %errorlevel% neq 0 (
        echo   WARNING: No C compiler (cl.exe / gcc) found on PATH.
        echo   mp3lame-sys needs a C compiler for the vendored LAME build.
        echo   If the build fails, install Visual Studio Build Tools or MSYS2.
        echo.
    ) else (
        for /f "tokens=*" %%g in ('gcc --version 2^>nul ^| findstr /i "gcc"') do (
            echo   OK: %%g
        )
    )
) else (
    echo   OK: MSVC cl.exe found
)
echo.

:: ── 3. Build release binary ──
echo [3/4] Building Baxter's Stereo Mix (release)...
echo   This may take a few minutes on the first run.
echo.
cargo build --release 2>&1
if %errorlevel% neq 0 (
    echo.
    echo   ERROR: Build failed. Check the output above.
    pause
    exit /b 1
)
echo.
echo   Build successful!
echo.

:: ── 4. Create Start Menu shortcut ──
echo [4/4] Creating Start Menu shortcut...
set "STARTMENU=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Baxters Office Suite"
if not exist "%STARTMENU%" mkdir "%STARTMENU%"

set "EXE_PATH=%BSM_ROOT%target\release\bsm-ui.exe"
if not exist "!EXE_PATH!" (
    echo   WARNING: bsm-ui.exe not found at !EXE_PATH!
    echo   Skipping shortcut creation.
) else (
    powershell -Command "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('%STARTMENU%\Baxters Stereo Mix.lnk'); $s.TargetPath = '!EXE_PATH!'; $s.WorkingDirectory = '%BSM_ROOT%'; $s.Description = 'Baxters Stereo Mix - Audio Capture'; $s.Save()"
    if %errorlevel% equ 0 (
        echo   OK: Shortcut created in Start Menu ^> Baxters Office Suite
    ) else (
        echo   WARNING: Shortcut creation failed. You can run bsm-ui.exe directly.
    )
)

echo.
echo ============================================================
echo   Installation complete!
echo   Binary: %BSM_ROOT%target\release\bsm-ui.exe
echo ============================================================
echo.
pause
