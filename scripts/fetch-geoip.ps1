# 下载 DB-IP Country Lite（MMDB，CC BY 4.0）到 src-tauri/resources/geoip。
# 离线库让归属地查询在断网时仍能显示国家；缺失时应用自动回落在线的 ipwho.is。
#
# 默认取当月版本，例如 2026-11；可用 -Month 指定历史月份。

param(
    [string]$Month = (Get-Date -Format 'yyyy-MM')
)

$ErrorActionPreference = 'Stop'

$File    = "dbip-country-lite-$Month.mmdb"
$Url     = "https://download.db-ip.com/free/$File.gz"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$GeoDir  = Join-Path $RepoRoot 'src-tauri\resources\geoip'
$Dest    = Join-Path $GeoDir 'dbip-country-lite.mmdb'
$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) "vpm-search-geoip-$Month"

New-Item -ItemType Directory -Force -Path $GeoDir | Out-Null
if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$Gz = Join-Path $WorkDir "$File.gz"
Write-Host "下载 DB-IP Country Lite $Month：$Url"
Invoke-WebRequest -Uri $Url -OutFile $Gz -UseBasicParsing

# 解 gzip：.NET 的 GzipStream 在 Windows PowerShell 5.1 与 PowerShell 7 上都能用。
$input = [System.IO.File]::OpenRead($Gz)
try {
    $output = [System.IO.File]::Create($Dest)
    try {
        $gzip = New-Object System.IO.Compression.GzipStream(
            $input, [System.IO.Compression.CompressionMode]::Decompress)
        try { $gzip.CopyTo($output) } finally { $gzip.Dispose() }
    } finally { $output.Dispose() }
} finally { $input.Dispose() }

Remove-Item -Recurse -Force $WorkDir

$size = (Get-Item $Dest).Length
Write-Host "已放置：$Dest（$([math]::Round($size / 1MB, 1)) MB）" -ForegroundColor Green
