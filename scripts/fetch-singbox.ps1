# 下载固定版本的 sing-box，解出 Windows amd64 可执行文件放到 src-tauri/binaries。
# 该二进制不进版本库（见 .gitignore），首次克隆后必须先跑一次本脚本。

$ErrorActionPreference = 'Stop'

$Version = '1.14.4'
$Triple  = 'x86_64-pc-windows-msvc'
$Asset   = "sing-box-$Version-windows-amd64.zip"
$Url     = "https://github.com/SagerNet/sing-box/releases/download/v$Version/$Asset"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$BinDir   = Join-Path $RepoRoot 'src-tauri\binaries'
$Dest     = Join-Path $BinDir "sing-box-$Triple.exe"
$WorkDir  = Join-Path ([System.IO.Path]::GetTempPath()) "vpm-search-singbox-$Version"

if (Test-Path $Dest) {
    Write-Host "已存在：$Dest" -ForegroundColor Yellow
    & $Dest version
    exit 0
}

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$Zip = Join-Path $WorkDir $Asset
Write-Host "下载 sing-box $Version：$Url"
Invoke-WebRequest -Uri $Url -OutFile $Zip -UseBasicParsing

Write-Host "解压到 $WorkDir"
Expand-Archive -Path $Zip -DestinationPath $WorkDir -Force

$Exe = Get-ChildItem -Path $WorkDir -Recurse -Filter 'sing-box.exe' |
    Select-Object -First 1
if (-not $Exe) {
    throw "压缩包里没有找到 sing-box.exe"
}

Copy-Item -Path $Exe.FullName -Destination $Dest -Force
Remove-Item -Recurse -Force $WorkDir

Write-Host "已放置：$Dest" -ForegroundColor Green
& $Dest version
