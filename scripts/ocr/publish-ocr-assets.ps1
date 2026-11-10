#requires -Version 7.0
<#
.SYNOPSIS
    把 OCR 模型与 onnxruntime.dll 同时发布到 GitHub Release 和 Gitee Release，
    并生成带双源 URL 的 ocr-manifest.json。

.DESCRIPTION
    骨架脚本：流程完整，但没有在真实仓库上跑过（本机缺 GITEE_TOKEN / GH_TOKEN）。
    真正执行前需要：
      1) scripts/ocr/models/ 下放齐 4 个文件（3 个 onnx + onnxruntime.dll）
      2) GitHub 侧已 gh auth（或用 GH_TOKEN），Gitee 侧 GITEE_TOKEN 具备 projects 权限
      3) 先建好 Gitee 镜像仓库，GITEE_OWNER / GITEE_REPO 指向它
      4) gh >= 2.20（用到 --latest=false，避免 OCR Release 抢走「最新版本」）

    只生成清单、不上传：  ./publish-ocr-assets.ps1 -Version 1.0.0 -DryRun
    跳过 Gitee：          ./publish-ocr-assets.ps1 -Version 1.0.0 -SkipGitee

.NOTES
    清单最后上传：应用只有在所有文件都可下载后才会看到新版本，避免半套发布。
    Gitee 没有官方 CLI，这里直接走 REST v5。注意 attach_files 不解析 multipart 表单体里的
    access_token（会报 401 / code 40001），令牌必须放在查询参数上，因此附件用 curl -F 上传。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [string]$FilesDir = (Join-Path $PSScriptRoot 'models'),
    [string]$GitHubRepo = 'KrisShin/Bead',

    [string]$GiteeOwner = $env:GITEE_OWNER,
    [string]$GiteeRepo = $(if ([string]::IsNullOrWhiteSpace($env:GITEE_REPO)) { 'Bead' } else { $env:GITEE_REPO }),
    [string]$GiteeToken = $env:GITEE_TOKEN,

    [switch]$SkipGitee,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$tag = "ocr-v$Version"
$releaseTitle = "OCR assets $Version"
$releaseNotes = @"
内置 OCR 资源（PP-OCRv4 mobile + ONNX Runtime）。
文件校验值见同 Release 的 ocr-manifest.json。
"@

# 清单里出现哪些文件、每个文件属于哪种角色、覆盖哪些语言（* = 语言无关）。
# 新增语种时在这里加 rec 模型，并同步 bundles 的分组。
$catalog = @(
    [pscustomobject]@{ Name = 'ch_PP-OCRv4_det_infer.onnx';         Kind = 'det';     Languages = @('*') }
    [pscustomobject]@{ Name = 'ch_PP-OCRv4_rec_infer.onnx';         Kind = 'rec';     Languages = @('zh', 'en') }
    [pscustomobject]@{ Name = 'ch_ppocr_mobile_v2.0_cls_infer.onnx'; Kind = 'cls';     Languages = @('*') }
    [pscustomobject]@{ Name = 'onnxruntime.dll';                     Kind = 'runtime'; Languages = @('*') }
)

# 一个 bundle 就是设置页里一次「下载/启用 OCR」要拉的全集。
$bundles = @(
    [pscustomobject]@{
        Id        = 'zh_en'
        Languages = @('zh', 'en')
        Files     = @(
            'ch_PP-OCRv4_det_infer.onnx'
            'ch_PP-OCRv4_rec_infer.onnx'
            'ch_ppocr_mobile_v2.0_cls_infer.onnx'
            'onnxruntime.dll'
        )
    }
)

if (-not (Test-Path $FilesDir)) { throw "模型目录不存在：$FilesDir" }
if ([string]::IsNullOrWhiteSpace($GiteeOwner)) {
    Write-Warning 'GITEE_OWNER 未设置：清单里的 Gitee URL 会是 YOUR_GITEE_OWNER 占位符。'
    $GiteeOwner = 'YOUR_GITEE_OWNER'
}

$files = foreach ($item in $catalog) {
    $path = Join-Path $FilesDir $item.Name
    if (-not (Test-Path $path)) { throw "缺少文件：$path" }
    [pscustomobject]@{
        Name      = $item.Name
        Kind      = $item.Kind
        Languages = $item.Languages
        Size      = (Get-Item $path).Length
        Sha256    = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant()
        Path      = $path
        Sources   = @(
            "https://github.com/$GitHubRepo/releases/download/$tag/$($item.Name)"
            "https://gitee.com/$GiteeOwner/$GiteeRepo/releases/download/$tag/$($item.Name)"
        )
    }
}

$bundleManifest = foreach ($bundle in $bundles) {
    $total = ($files | Where-Object { $bundle.Files -contains $_.Name } | Measure-Object -Property Size -Sum).Sum
    [ordered]@{
        id         = $bundle.Id
        languages  = $bundle.Languages
        files      = $bundle.Files
        total_size = [int64]$total
    }
}

$manifest = [ordered]@{
    schema_version  = 1
    engine          = [ordered]@{
        id              = 'ppocr-v4-mobile'
        runtime         = 'onnxruntime'
        runtime_version = '1.31.1'
    }
    version         = $Version
    tag             = $tag
    released_at     = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    min_app_version = '0.1.0'
    bundles         = @($bundleManifest)
    files           = @(
        $files | ForEach-Object {
            [ordered]@{
                name      = $_.Name
                kind      = $_.Kind
                languages = $_.Languages
                size      = [int64]$_.Size
                sha256    = $_.Sha256
                sources   = $_.Sources
            }
        }
    )
}

$manifestPath = Join-Path $FilesDir 'ocr-manifest.json'
[System.IO.File]::WriteAllText(
    $manifestPath,
    ($manifest | ConvertTo-Json -Depth 8),
    [System.Text.UTF8Encoding]::new($false)
)
Write-Host "清单已生成：$manifestPath" -ForegroundColor Green

if ($DryRun) {
    Write-Host 'DryRun：跳过上传与在线校验。' -ForegroundColor Yellow
    return
}

# ---------------- GitHub ----------------
function Invoke-Gh {
    param([string[]]$GhArgs)
    & gh @GhArgs
    if ($LASTEXITCODE -ne 0) { throw "gh $($GhArgs -join ' ') 失败（exit $LASTEXITCODE）" }
}

if ([string]::IsNullOrWhiteSpace($env:GH_TOKEN) -and [string]::IsNullOrWhiteSpace($env:GITHUB_TOKEN)) {
    Write-Warning 'GH_TOKEN / GITHUB_TOKEN 未设置，依赖 gh 的本地登录态。'
}

& gh release view $tag --repo $GitHubRepo *> $null
if ($LASTEXITCODE -ne 0) {
    Invoke-Gh -GhArgs @(
        'release', 'create', $tag,
        '--repo', $GitHubRepo,
        '--title', $releaseTitle,
        '--notes', $releaseNotes,
        '--latest=false'
    )
}

# 先传文件，最后传清单。
$filePaths = @($files | ForEach-Object { $_.Path })
Invoke-Gh -GhArgs (@('release', 'upload', $tag, '--repo', $GitHubRepo, '--clobber') + $filePaths)
Invoke-Gh -GhArgs @('release', 'upload', $tag, '--repo', $GitHubRepo, '--clobber', $manifestPath)
Write-Host 'GitHub：文件与清单已上传。' -ForegroundColor Green

# ---------------- Gitee ----------------
if ($SkipGitee) {
    Write-Host '按参数跳过 Gitee。' -ForegroundColor Yellow
} elseif ([string]::IsNullOrWhiteSpace($GiteeToken)) {
    Write-Warning 'GITEE_TOKEN 未设置，跳过 Gitee 上传（清单中的 Gitee URL 暂不可用）。'
} else {
    $apiBase = 'https://gitee.com/api/v5'
    $repoPath = "/repos/$GiteeOwner/$GiteeRepo"

    function Get-GiteeRelease {
        try {
            Invoke-RestMethod -Method Get -Uri "$apiBase$repoPath/releases/tags/$tag" -TimeoutSec 30
        } catch {
            $null
        }
    }

    $release = Get-GiteeRelease
    if ($null -eq $release) {
        $release = Invoke-RestMethod -Method Post -Uri "$apiBase$repoPath/releases" -Body @{
            access_token     = $GiteeToken
            tag_name         = $tag
            name             = $releaseTitle
            body             = $releaseNotes
            target_commitish = 'main'
            prerelease       = $false
        } -TimeoutSec 60
    }

    # Gitee 的 attach_files 不解析 multipart 表单体里的 access_token，
    # 必须放在查询参数上；这里用 curl -F 上传，避免 Invoke-RestMethod -Form 的 401。
    function Send-GiteeAsset {
        param([string]$Path)

        $name = Split-Path $Path -Leaf
        $attachBase = "$apiBase$repoPath/releases/$($release.id)/attach_files"

        # 同名附件会冲突：先删除旧附件；删不掉就中止，避免「看起来成功、实际仍是旧文件」。
        $current = Invoke-RestMethod -Method Get -Uri "$apiBase$repoPath/releases/$($release.id)" -TimeoutSec 30
        $old = @($current.assets) | Where-Object { $_.name -eq $name } | Select-Object -First 1
        if ($old) {
            try {
                Invoke-RestMethod -Method Delete -Uri "$attachBase/$($old.id)?access_token=$GiteeToken" -TimeoutSec 30 | Out-Null
            } catch {
                $safe = $_.Exception.Message -replace [regex]::Escape($GiteeToken), '<redacted>'
                Write-Warning "Gitee 同名附件删除失败：$name（attach id $($old.id)）：$safe"
            }

            $check = Invoke-RestMethod -Method Get -Uri "$apiBase$repoPath/releases/$($release.id)" -TimeoutSec 30
            if (@($check.assets) | Where-Object { $_.name -eq $name }) {
                throw "Gitee 同名附件仍存在且删除失败：$name（attach id $($old.id)）。请先在网页端删除该附件再重跑；脚本不会覆盖上传，以免清单与实际附件不一致。"
            }
        }

        $raw = & curl.exe -sS -w "`n%{http_code}" -X POST "${attachBase}?access_token=$GiteeToken" --form "file=@$Path" 2>&1
        $curlExit = $LASTEXITCODE
        $lines = ($raw | Out-String).TrimEnd() -split "`r?`n"
        $http = $lines[-1]
        $body = ($lines[0..([Math]::Max(0, $lines.Count - 2))] -join "`n")
        if ($curlExit -ne 0 -or $http -notmatch '^2') {
            $safe = $body -replace [regex]::Escape($GiteeToken), '<redacted>'
            throw "Gitee 上传失败：$name（curl exit $curlExit，HTTP $http）$safe"
        }
        Write-Host "Gitee：已上传 $name" -ForegroundColor Green
    }

    foreach ($file in $files) {
        Send-GiteeAsset -Path $file.Path
    }
    Send-GiteeAsset -Path $manifestPath
    Write-Host 'Gitee：文件与清单已上传。' -ForegroundColor Green
}

# ---------------- 在线校验 ----------------
# GitHub 支持 Range（206 + Content-Range）；Gitee 忽略 Range 直接 200 全量，
# 此时退化为比对 Content-Length。HEAD 在 Gitee 可能 403，所以统一用 GET。
foreach ($file in $files) {
    foreach ($url in $file.Sources) {
        if ($url -match 'YOUR_GITEE_OWNER') { continue }
        try {
            $response = Invoke-WebRequest -Uri $url -Headers @{ Range = 'bytes=0-0' } -MaximumRedirection 5 -TimeoutSec 30
            $range = $response.Headers['Content-Range']
            if ($range -and ($range -match '/(\d+)$')) {
                $total = [int64]$Matches[1]
            } else {
                $total = [int64]$response.Headers['Content-Length']
            }
            $ok = ($total -eq $file.Size)
            $status = if ($ok) { "OK（$total B）" } else { "长度不符：$total（期望 $($file.Size)）" }
            Write-Host ("{0,-40} {1}" -f $file.Name, $status)
        } catch {
            Write-Warning "$($file.Name) 校验失败：$url — $($_.Exception.Message)"
        }
    }
}

Write-Host "完成。tag=$tag，清单：$manifestPath" -ForegroundColor Green
