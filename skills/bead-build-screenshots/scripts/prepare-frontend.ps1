#requires -Version 7.0
<#
把 Tauri 构建实际嵌入的 web 资源做成可复现的截图输入。

背景：本仓库 tauri.conf.json 的 build.frontendDist 指向 ../web，没有独立打包器。
Tauri 会把该目录逐字节嵌入可执行文件，因此「编译产物里的前端包」就是构建时的 web/
目录。本脚本在构建之后复制这份目录，记录每个文件的 SHA-256 与整树哈希，并核对
exe 的版本与时间，确保截图用的资源就是当前产物所用的那一份。

用法：
  pwsh -File prepare-frontend.ps1                 # 只校验并暂存（要求 exe 已构建且不旧于 web）
  pwsh -File prepare-frontend.ps1 -Build          # 先跑 release 构建（仅 exe），再暂存
  pwsh -File prepare-frontend.ps1 -Exe target/debug/bead.exe
#>
[CmdletBinding()]
param(
    [switch]$Build,
    [string]$Exe,
    [string]$OutRoot
)

$ErrorActionPreference = 'Stop'

function Find-RepoRoot {
    $dir = (Get-Location).Path
    while ($dir) {
        if (Test-Path (Join-Path $dir 'src-tauri\tauri.conf.json')) { return $dir }
        $parent = Split-Path $dir -Parent
        if (-not $parent -or $parent -eq $dir) { break }
        $dir = $parent
    }
    throw '找不到 Bead 仓库根目录，请在仓库内运行。'
}

function Get-TreeManifest {
    param([string]$Root, [string]$Prefix = '')
    $entries = [System.Collections.Generic.List[object]]::new()
    Get-ChildItem $Root -Recurse -File | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring((Resolve-Path $Root).Path.Length + 1).Replace('\', '/')
        if ($Prefix) { $relative = "$Prefix/$relative" }
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $entries.Add([pscustomobject]@{
                Path   = $relative
                Size   = $_.Length
                Sha256 = $hash
            })
    }
    return $entries
}

function Get-TreeHash {
    param($Entries)
    $builder = [System.Text.StringBuilder]::new()
    foreach ($entry in ($Entries | Sort-Object Path)) {
        [void]$builder.Append($entry.Path).Append("`0").Append($entry.Sha256).Append("`n")
    }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
        return ([System.BitConverter]::ToString($sha.ComputeHash($bytes)) -replace '-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}

$root = Find-RepoRoot
$confPath = Join-Path $root 'src-tauri\tauri.conf.json'
$conf = Get-Content $confPath -Raw | ConvertFrom-Json
$version = [string]$conf.version
$frontendDist = [string]$conf.build.frontendDist
if (-not $frontendDist) { throw 'tauri.conf.json 缺少 build.frontendDist。' }

$webDir = (Resolve-Path (Join-Path (Join-Path $root 'src-tauri') $frontendDist)).Path
if (-not (Test-Path (Join-Path $webDir 'index.html'))) { throw "frontendDist 不是有效的 web 目录：$webDir" }

if ($Build) {
    Write-Host "=== 构建 release exe（npx tauri build --no-bundle）===" -ForegroundColor Cyan
    Push-Location $root
    try {
        npx tauri build --no-bundle
        if ($LASTEXITCODE -ne 0) { throw "tauri build 失败（exit $LASTEXITCODE）" }
    } finally {
        Pop-Location
    }
}

$exePath = if ($Exe) {
    if ([System.IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $root $Exe }
} else {
    Join-Path $root 'target\release\bead.exe'
}
if (-not (Test-Path $exePath)) {
    throw "找不到构建产物：$exePath。先运行 npm run build，或给本脚本加 -Build。"
}
$exeItem = Get-Item $exePath
$exeVersion = $exeItem.VersionInfo.ProductVersion
if (-not $exeVersion -or -not ($exeVersion -like "$version*")) {
    throw "exe 版本（$exeVersion）与 tauri.conf 版本（$version）不一致，产物不是当前要发布的那份。"
}

$newestWeb = Get-ChildItem $webDir -Recurse -File | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestWeb -and $exeItem.LastWriteTime -lt $newestWeb.LastWriteTime.AddSeconds(-1)) {
    throw "exe 构建时间（$($exeItem.LastWriteTime)）早于最新 web 资源（$($newestWeb.LastWriteTime)）。重新构建，或使用 -Build。"
}

$files = Get-TreeManifest -Root $webDir -Prefix 'web'
$treeHash = Get-TreeHash -Entries $files

$outRoot = if ($OutRoot) { $OutRoot } else { Join-Path $root '.screenshots\.build' }
$stagedRoot = Join-Path $outRoot ("frontend-$version-" + $treeHash.Substring(0, 8))
$stagedWeb = Join-Path $stagedRoot 'web'
if (Test-Path $stagedRoot) { Remove-Item $stagedRoot -Recurse -Force }
New-Item -ItemType Directory -Force $stagedWeb | Out-Null
Copy-Item (Join-Path $webDir '*') $stagedWeb -Recurse -Force

$stagedFiles = Get-TreeManifest -Root $stagedWeb -Prefix 'web'
$stagedHash = Get-TreeHash -Entries $stagedFiles
if ($stagedHash -ne $treeHash) { throw '暂存后的资源哈希与源目录不一致，复制过程可能被篡改或中断。' }

$gitCommit = $null
$gitDirtyWeb = $null
try {
    Push-Location $root
    $gitCommit = (git rev-parse HEAD 2>$null)
    $dirty = (git status --porcelain -- web 2>$null)
    $gitDirtyWeb = [bool]($dirty -and $dirty.Trim())
} catch {
    # 非 git 工作区也允许出图，只是溯源信息为空。
} finally {
    Pop-Location
}

$manifest = [pscustomobject]@{
    schema       = 1
    createdAt    = (Get-Date).ToString('o')
    appVersion   = $version
    source       = 'tauri-frontendDist'
    frontendDist = $frontendDist
    webDir       = $webDir
    treeHash     = $treeHash
    fileCount    = @($files).Count
    files        = @($files)
    gitCommit    = $gitCommit
    gitDirtyWeb  = $gitDirtyWeb
    artifact     = [pscustomobject]@{
        path    = $exeItem.FullName
        size    = $exeItem.Length
        sha256  = (Get-FileHash $exeItem.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        version = $exeVersion
        mtime   = $exeItem.LastWriteTime.ToString('o')
    }
    stagedWebDir = $stagedWeb
}
$manifestPath = Join-Path $stagedRoot 'manifest.json'
$manifest | ConvertTo-Json -Depth 8 | Set-Content -Path $manifestPath -Encoding utf8

Write-Output "已暂存构建资源：$stagedWeb"
Write-Output "树哈希：$treeHash（$($manifest.fileCount) 个文件）"
Write-Output "产物：$($exeItem.FullName) v$exeVersion"
Write-Output "清单：$manifestPath"
$manifest | ConvertTo-Json -Depth 6
