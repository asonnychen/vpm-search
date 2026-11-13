# 生成免安装 zip：把主程序、内置 sing-box、离线归属地库与许可声明打成一个目录再压缩。
# 前置：先执行 pnpm tauri build 产出 src-tauri/target/release/vpm-search.exe。
# 用法：pwsh -File scripts/package-portable.ps1 [-SkipBuild]

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$Conf     = Get-Content (Join-Path $RepoRoot 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
$Version  = $Conf.version
$Name     = "$($Conf.productName)-$Version-portable"
$Release  = Join-Path $RepoRoot 'src-tauri\target\release'
$Exe      = Join-Path $Release 'vpm-search.exe'
$Bin      = Join-Path $RepoRoot 'src-tauri\binaries\sing-box-x86_64-pc-windows-msvc.exe'
$GeoDir   = Join-Path $RepoRoot 'src-tauri\resources\geoip'
$Notices  = Join-Path $RepoRoot 'THIRD_PARTY_NOTICES.md'
$Readme   = Join-Path $RepoRoot 'README.md'
$Stage    = Join-Path $RepoRoot "src-tauri\target\portable\$Name"
$OutDir   = Join-Path $RepoRoot 'src-tauri\target\release\bundle\portable'
$Zip      = Join-Path $OutDir "$Name.zip"

if (-not $SkipBuild -and -not (Test-Path $Exe)) {
    Write-Host '未找到主程序，先执行 pnpm tauri build ...'
    Push-Location $RepoRoot
    try { pnpm tauri build --no-bundle } finally { Pop-Location }
}

foreach ($required in @($Exe, $Bin, $Notices, $Readme)) {
    if (-not (Test-Path $required)) { throw "缺少必需文件：$required" }
}
$GeoDb = Join-Path $GeoDir 'dbip-country-lite.mmdb'
if (-not (Test-Path $GeoDb)) {
    throw "缺少离线归属地库：$GeoDb（先执行 pwsh -File scripts/fetch-geoip.ps1）"
}

if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage, (Join-Path $Stage 'geoip'), $OutDir | Out-Null

Copy-Item $Exe     (Join-Path $Stage 'vpm-search.exe')
Copy-Item $Bin     (Join-Path $Stage 'sing-box-x86_64-pc-windows-msvc.exe')
Copy-Item $GeoDb   (Join-Path $Stage 'geoip\dbip-country-lite.mmdb')
Copy-Item $Notices (Join-Path $Stage 'THIRD_PARTY_NOTICES.md')
Copy-Item $Readme  (Join-Path $Stage 'README.md')

if (Test-Path $Zip) { Remove-Item -Force $Zip }
Compress-Archive -Path $Stage -DestinationPath $Zip -CompressionLevel Optimal

$size = [math]::Round((Get-Item $Zip).Length / 1MB, 1)
Write-Host "已生成免安装包：$Zip（$size MB）" -ForegroundColor Green
