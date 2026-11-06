# Bead 一键打包：依赖自举 → 全量测试 → tauri release 构建 → 产物清单。
# 用法：
#   powershell -File build.ps1            # 完整流程（测试 + 打包）
#   powershell -File build.ps1 -SkipTests # 跳过测试直接打包（出包快）
# 产物：target/release/bead.exe（裸 exe，目录形态便携可用）与
#       target/release/bundle/nsis/Bead_<version>_x64-setup.exe（Windows 安装器）。
# 说明：Tauri v2 没有 zip bundle 类型，便携目录就是裸 exe 所在目录，本脚本不额外压 zip。
param([switch]$SkipTests)

$ErrorActionPreference = 'Stop'
# PowerShell 7 下 cargo/npm 把编译进度写到 stderr 属正常，不应因此中断脚本；
# Windows PowerShell 5.1 没有此变量，赋值只是创建一个普通变量，无害。
$PSNativeCommandUseErrorActionPreference = $false
Set-Location -Path $PSScriptRoot

function Assert-LastExit {
    param([string]$Stage)
    if ($LASTEXITCODE -ne 0) { throw "$Stage 失败（exit $LASTEXITCODE）" }
}

# 工具链缺失时给可读错误，不要让 native 命令报「不是内部或外部命令」。
foreach ($tool in 'node', 'npm', 'cargo') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "缺少 $tool，请先安装（见 README「从源码构建」）"
    }
}

Write-Host '=== [1/5] 前端依赖 ===' -ForegroundColor Cyan
# 干净 clone / 换机器后没有 node_modules：npm test 找不到 vitest、npx tauri 会去联网拉 CLI，
# 两者都会让「一键打包」在第一步就失败。缺依赖时先自举，再进入测试与构建。
if (-not (Test-Path 'node_modules/.bin/vitest*') -or -not (Test-Path 'node_modules/.bin/tauri*')) {
    Write-Host '未检测到本地依赖，执行 npm ci ...'
    npm ci --no-audit --no-fund
    Assert-LastExit 'npm ci'
} else {
    Write-Host '已就绪（跳过安装）'
}

Write-Host '=== [2/5] 前端测试 ===' -ForegroundColor Cyan
if (-not $SkipTests) {
    npm test
    Assert-LastExit '前端测试'
} else { Write-Host '（跳过）' }

Write-Host '=== [3/5] Rust 测试（bead-core / bead-providers / shell）===' -ForegroundColor Cyan
if (-not $SkipTests) {
    cargo test --manifest-path bead-core/Cargo.toml
    Assert-LastExit 'bead-core 测试'
    cargo test -p bead-providers
    Assert-LastExit 'bead-providers 测试'
    cargo test -p bead
    Assert-LastExit 'shell 测试'
} else { Write-Host '（跳过）' }

Write-Host '=== [4/5] tauri build（release 编译 + NSIS 打包，首次约 5-10 分钟）===' -ForegroundColor Cyan
npx tauri build
Assert-LastExit 'tauri build'

Write-Host '=== [5/5] 产物 ===' -ForegroundColor Green
# 尊重自定义 CARGO_TARGET_DIR，否则「构建成功但清单报找不到文件」会被误判为打包失败。
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
$exe = Join-Path (Join-Path $targetDir 'release') 'bead.exe'
$bundle = Join-Path (Join-Path $targetDir 'release') 'bundle'
if (-not (Test-Path $exe)) { throw "未找到裸 exe：$exe（tauri build 已返回但产物缺失）" }
if (-not (Test-Path $bundle)) { throw "未找到安装包目录：$bundle" }
Get-ChildItem -Recurse $bundle -File | ForEach-Object {
    Write-Host ('  ' + $_.FullName + '  (' + [math]::Round($_.Length / 1MB, 1) + ' MB)')
}
Write-Host ('  裸 exe：' + (Resolve-Path $exe))
Write-Host '完成。'
