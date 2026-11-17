@echo off
rem DivertLink 控制台启动器（Windows 可双击运行）。
rem
rem 查找顺序：%DIVERTLINK_BIN%、PATH 中的 divertlink、仓库内 target\release\divertlink、
rem target\debug\divertlink。启动后自动打开默认浏览器。

setlocal EnableExtensions
cd /d "%~dp0"

set "BIN="
if defined DIVERTLINK_BIN if exist "%DIVERTLINK_BIN%" set "BIN=%DIVERTLINK_BIN%"
if not defined BIN (
  for %%I in (divertlink.exe) do if not "%%~$PATH:I"=="" set "BIN=%%~$PATH:I"
)
if not defined BIN if exist "..\..\target\release\divertlink.exe" set "BIN=..\..\target\release\divertlink.exe"
if not defined BIN if exist "..\..\target\debug\divertlink.exe" set "BIN=..\..\target\debug\divertlink.exe"

if not defined BIN (
  echo 未找到 divertlink.exe。
  echo 请先运行 cargo build --release，或设置 DIVERTLINK_BIN 指向二进制。
  pause
  exit /b 1
)

echo 正在启动 DivertLink 控制台：http://127.0.0.1:17653/
"%BIN%" console --open
echo.
echo 控制台已退出。
pause
