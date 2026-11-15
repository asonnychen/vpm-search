$ErrorActionPreference = "Stop"
Set-Location "E:\my\ai-tools\vpm-search"

$nsis = "src-tauri\target\release\bundle\nsis\vpm-search_0.1.1_x64-setup.exe"
$zip = "src-tauri\target\release\bundle\portable\vpm-search-0.1.1-portable.zip"
$sums = "src-tauri\target\release\bundle\SHA256SUMS.txt"

Get-FileHash -Path @($nsis, $zip) -Algorithm SHA256 |
  ForEach-Object { "$($_.Hash.ToLower())  $(Split-Path $_.Path -Leaf)" } |
  Set-Content -Path $sums -Encoding ascii

git add -A
git -c user.name="vpm-search" -c user.email="dev@local" commit -m "chore(release): 0.1.1" 2>&1 | Out-Null
git tag -a v0.1.1 -m "vpm-search 0.1.1"
git push origin main 2>&1
git push origin v0.1.1 2>&1

gh release create v0.1.1 $nsis $zip $sums --title "vpm-search 0.1.1" --notes-file "C:\Users\Administrator\AppData\Local\Temp\opencode\release-notes-011.md"
