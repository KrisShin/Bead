# Bead 一键打包：测试 → release 构建 → 各平台安装包/便携包（在 Windows 上产出 NSIS + zip）。
# 用法：
#   powershell -File build.ps1            # 完整流程（测试 + 打包）
#   powershell -File build.ps1 -SkipTests # 跳过测试直接打包（出包快）
# 产物位置：target/release/bead.exe（裸 exe）与 target/release/bundle/（安装器与压缩包）。
param([switch]$SkipTests)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

Write-Host '=== [1/4] 前端测试 ===' -ForegroundColor Cyan
if (-not $SkipTests) { npm test; if ($LASTEXITCODE -ne 0) { throw '前端测试未通过' } } else { Write-Host '（跳过）' }

Write-Host '=== [2/4] Rust 测试（bead-core / bead-providers / shell）===' -ForegroundColor Cyan
if (-not $SkipTests) {
    cargo test --manifest-path bead-core/Cargo.toml; if ($LASTEXITCODE -ne 0) { throw 'bead-core 测试未通过' }
    cargo test -p bead-providers; if ($LASTEXITCODE -ne 0) { throw 'bead-providers 测试未通过' }
    cargo test -p bead; if ($LASTEXITCODE -ne 0) { throw 'shell 测试未通过' }
} else { Write-Host '（跳过）' }

Write-Host '=== [3/4] tauri build（release 编译 + 打包，首次约 5-10 分钟）===' -ForegroundColor Cyan
npx tauri build
if ($LASTEXITCODE -ne 0) { throw 'tauri build 失败' }

Write-Host '=== [4/4] 产物 ===' -ForegroundColor Green
Get-ChildItem -Recurse 'target\release\bundle' -File | ForEach-Object { Write-Host ('  ' + $_.FullName + '  (' + [math]::Round($_.Length / 1MB, 1) + ' MB)') }
Write-Host ('  裸 exe：' + (Resolve-Path 'target\release\bead.exe'))
Write-Host '完成。'
