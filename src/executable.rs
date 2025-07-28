use crate::platform::Platform;
use anyhow::{Context, Result};
use base64::Engine as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

/// Create a self-extracting executable for the current platform
pub fn create_self_extracting_executable(
    out: &Path,
    zip_data: Vec<u8>,
    _app_name: &str,
) -> Result<()> {
    let build_id = Uuid::new_v4();

    if Platform::current().is_windows() {
        create_windows_executable(out, zip_data, &build_id.to_string())
    } else {
        create_unix_executable(out, zip_data, &build_id.to_string())
    }
}

/// Create a Unix-compatible self-extracting executable
fn create_unix_executable(out: &Path, zip_data: Vec<u8>, build_id: &str) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let mut file = fs::File::create(out).context("Failed to create output executable")?;

    let script = format!(
        r#"#!/bin/bash
set -e

if [ -n "$XDG_CACHE_HOME" ]; then
    CACHE_DIR="$XDG_CACHE_HOME/banderole"
elif [ -n "$HOME" ]; then
    CACHE_DIR="$HOME/.cache/banderole"
else
    CACHE_DIR="/tmp/banderole-cache"
fi

APP_DIR="$CACHE_DIR/{build_id}"
READY_FILE="$APP_DIR/.ready"

run_app() {{
    cd "$APP_DIR/app" || exit 1
    
    # Find main script from package.json
    MAIN_SCRIPT=$("$APP_DIR/node/bin/node" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2>/dev/null || echo "index.js")
    
    if [ -f "$MAIN_SCRIPT" ]; then
        exec "$APP_DIR/node/bin/node" "$MAIN_SCRIPT" "$@"
    else
        echo "Error: Main script '$MAIN_SCRIPT' not found" >&2
        exit 1
    fi
}}

if [ -f "$READY_FILE" ] && [ -f "$APP_DIR/app/package.json" ] && [ -x "$APP_DIR/node/bin/node" ]; then
    run_app "$@"
fi

mkdir -p "$CACHE_DIR" 2>/dev/null || true

ATTEMPTS=0
MAX_ATTEMPTS=30

while [ $ATTEMPTS -lt $MAX_ATTEMPTS ]; do
    if mkdir "$APP_DIR" 2>/dev/null; then
        
        TEMP_ZIP=$(mktemp) || exit 1
        trap 'rm -f "$TEMP_ZIP"' EXIT
        
        awk '/^__DATA__$/{{p=1;next}} p{{print}}' "$0" | base64 -d > "$TEMP_ZIP"
        
        if [ ! -s "$TEMP_ZIP" ]; then
            echo "Error: Failed to extract bundle data" >&2
            rm -rf "$APP_DIR" 2>/dev/null || true
            exit 1
        fi
        
        if ! unzip -q "$TEMP_ZIP" -d "$APP_DIR" 2>/dev/null; then
            echo "Error: Failed to extract bundle" >&2
            rm -rf "$APP_DIR" 2>/dev/null || true
            exit 1
        fi
        
        if [ ! -f "$APP_DIR/app/package.json" ] || [ ! -x "$APP_DIR/node/bin/node" ]; then
            echo "Error: Bundle extraction incomplete" >&2
            rm -rf "$APP_DIR" 2>/dev/null || true
            exit 1
        fi
        
        touch "$READY_FILE"
        
        run_app "$@"
    else
        if [ -f "$READY_FILE" ] && [ -f "$APP_DIR/app/package.json" ] && [ -x "$APP_DIR/node/bin/node" ]; then
            run_app "$@"
        fi
        
        ATTEMPTS=$((ATTEMPTS + 1))
        if [ $ATTEMPTS -lt $MAX_ATTEMPTS ]; then
            DELAY=$(awk "BEGIN {{print int(100 * (2 ^ (($ATTEMPTS - 1) / 3)) + 0.5)}}" 2>/dev/null || echo 100)
            if [ "$DELAY" -gt 1000 ]; then
                DELAY=1000
            fi
            sleep $(awk "BEGIN {{print $DELAY / 1000}}" 2>/dev/null || echo 0.1)
        fi
    fi
done

# If we've exhausted retries, exit with error
echo "Error: Failed to extract or run application after $MAX_ATTEMPTS attempts" >&2
exit 1

__DATA__
"#
    );

    file.write_all(script.as_bytes())?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(&zip_data);
    file.write_all(encoded.as_bytes())?;
    file.write_all(b"\n")?;

    #[cfg(unix)]
    {
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(out, perms)?;
    }

    Ok(())
}

/// Create a Windows-compatible self-extracting executable
fn create_windows_executable(out: &Path, zip_data: Vec<u8>, build_id: &str) -> Result<()> {
    let bat_path = out.with_extension("bat");
    let mut file = fs::File::create(&bat_path).context("Failed to create output batch file")?;

    let script = format!(
        r#"@echo off
setlocal enabledelayedexpansion

set "CACHE_DIR=%LOCALAPPDATA%\banderole"
set "APP_DIR=!CACHE_DIR!\{build_id}"
set "LOCK_FILE=!APP_DIR!\.lock"
set "READY_FILE=!APP_DIR!\.ready"
set "QUEUE_DIR=!APP_DIR!\.queue"
set "EXTRACTION_PID_FILE=!APP_DIR!\.extraction.pid"

for /f "tokens=2 delims==" %%i in ('wmic process where "processid=%PID%" get processid /value 2^>nul ^| find "ProcessId"') do set "CURRENT_PID=%%i"
if "!CURRENT_PID!"=="" set "CURRENT_PID=%RANDOM%"

:run_app
cd /d "!APP_DIR!\app" || exit /b 1

for /f "delims=" %%i in ('"!APP_DIR!\node\node.exe" -e "try {{ console.log(require('./package.json').main ^|^| 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2^>nul') do set "MAIN_SCRIPT=%%i"
if "!MAIN_SCRIPT!"=="" set "MAIN_SCRIPT=index.js"

if exist "!MAIN_SCRIPT!" (
    "!APP_DIR!\node\node.exe" "!MAIN_SCRIPT!" %*
    exit /b !errorlevel!
) else (
    echo Error: Main script '!MAIN_SCRIPT!' not found >&2
    exit /b 1
)

:cleanup_queue
if defined QUEUE_ENTRY if exist "!QUEUE_ENTRY!" (
    del "!QUEUE_ENTRY!" 2>nul
)
exit /b

if exist "!READY_FILE!" if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
    goto run_app
)

if not exist "!QUEUE_DIR!" mkdir "!QUEUE_DIR!" 2>nul
set "QUEUE_ENTRY=!QUEUE_DIR!\!CURRENT_PID!-%RANDOM%-%TIME:~6,5%.queue"
echo !CURRENT_PID! > "!QUEUE_ENTRY!"

set /a WAIT_COUNT=0
set /a MAX_WAIT=120

:acquire_lock
call :cleanup_stale_locks

if not exist "!LOCK_FILE!" (
    mkdir "!LOCK_FILE!" 2>nul
    if !errorlevel! equ 0 (
        echo !CURRENT_PID! > "!EXTRACTION_PID_FILE!"
        goto extract
    )
)

:wait_for_ready
if exist "!READY_FILE!" (
    if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
        call :cleanup_queue
        goto run_app
    )
)

set /a WAIT_COUNT+=1
if !WAIT_COUNT! geq !MAX_WAIT! (
    echo Error: Timeout waiting for extraction to complete >&2
    call :cleanup_queue
    exit /b 1
)

if exist "!EXTRACTION_PID_FILE!" (
    timeout /t 1 /nobreak >nul 2>&1
    goto wait_for_ready
) else (
    timeout /t 1 /nobreak >nul 2>&1
    goto acquire_lock
)

:extract
if exist "!READY_FILE!" if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
    call :cleanup_queue
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    goto run_app
)

if not exist "!CACHE_DIR!" mkdir "!CACHE_DIR!"
if not exist "!APP_DIR!" mkdir "!APP_DIR!"

set "TEMP_ZIP=%TEMP%\banderole-bundle-!CURRENT_PID!-%RANDOM%.zip"
powershell -NoProfile -Command "$content = Get-Content '%~f0' -Raw; $dataStart = $content.IndexOf('__DATA__') + 8; $data = $content.Substring($dataStart).Trim(); [System.IO.File]::WriteAllBytes('!TEMP_ZIP!', [System.Convert]::FromBase64String($data))"

if not exist "!TEMP_ZIP!" (
    echo Error: Failed to extract bundle data >&2
    call :cleanup_queue
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

powershell -NoProfile -Command "try {{ Expand-Archive -Path '!TEMP_ZIP!' -DestinationPath '!APP_DIR!' -Force }} catch {{ Write-Error $_.Exception.Message; exit 1 }}"
set "EXTRACT_RESULT=!errorlevel!"
del "!TEMP_ZIP!" 2>nul

if !EXTRACT_RESULT! neq 0 (
    echo Error: Failed to extract bundle >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

if not exist "!APP_DIR!\app\package.json" (
    echo Error: Bundle extraction incomplete >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

if not exist "!APP_DIR!\node\node.exe" (
    echo Error: Node.js executable not found >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

echo ready > "!READY_FILE!"

if exist "!QUEUE_DIR!" (
    for %%f in ("!QUEUE_DIR!\*.queue") do (
        if exist "%%f" del "%%f" 2>nul
    )
)

del "!EXTRACTION_PID_FILE!" 2>nul
call :cleanup_queue
rmdir "!LOCK_FILE!" 2>nul

goto run_app

:cleanup_stale_locks
if exist "!LOCK_FILE!" (
    for /f %%i in ('powershell -NoProfile -Command "if (Test-Path '!LOCK_FILE!') {{ $age = (Get-Date) - (Get-Item '!LOCK_FILE!').CreationTime; if ($age.TotalSeconds -gt 300) {{ Write-Output 'stale' }} }}"') do (
        if "%%i"=="stale" (
            rmdir "!LOCK_FILE!" 2>nul
            del "!EXTRACTION_PID_FILE!" 2>nul
        )
    )
)
exit /b

__DATA__
"#
    );

    file.write_all(script.as_bytes())?;

    // Append base64-encoded zip data
    let encoded = base64::engine::general_purpose::STANDARD.encode(&zip_data);
    file.write_all(encoded.as_bytes())?;

    Ok(())
}
