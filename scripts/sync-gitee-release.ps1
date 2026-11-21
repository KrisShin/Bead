<#
sync-gitee-release.ps1 — 把 GitHub Release 镜像到 Gitee 同名仓库

作用
  GitHub Release 发布后（release-windows.yml 的 sync-gitee job），在 Gitee 建同名 tag 的 release，
  把安装包与校验文件作附件上传；本地用 -All 回填历史版本。

设计取舍
  - 只留最新版：开启 -PruneOld 后，同步完成会清掉**除保留版本外**所有 v* release 的附件，
    release 页与更新说明留作变更记录。目的是把用户摁在最新版上——用户拿着旧版报的 bug。
    必须先确认保留版本的附件齐了才动手，否则一个都下不了。
  - 不碰非软件版本：`ocr-v*` 这类资源发布的附件是已装用户下载模型的备源，清掉等于削减已有
    安装的可用性，清理只认 `^v\d+\.\d+\.\d+$`。
  - 幂等：Gitee 已有该 tag 的 release 时默认整条跳过（历史版本、ocr-v* 这类资源发布都不会被改写）；
    只有本次发布（-PublishedTag 或显式 -Tag）才补缺失附件、更新标题与正文。
  - 先 refs 后 release：脚本第一件事是把分支与本次 tag 推到 Gitee（`git push`，私人令牌走 HTTPS），
    然后才建 release。顺序反过来就是 400「创建标签失败」——Gitee 上没那个提交，tag 无处可指。
    分支一起推，Gitee 仓库的源码与 README 也跟着追平，不会出现「tag 是新的、main 是旧的」。
  - tag 落到正确提交：release 的 target_commitish 用 GitHub 上该 tag 解析出的 commit SHA
    （annotated tag 会剥一层），而不是拿 main 头凑一个错的。
  - 上传后按「文件名 + 字节数」复核，不一致删掉重传一次：Gitee 在跨境链路下偶发半包，
    半包比失败更坏——用户下载到打不开的安装包才发现。
  - 零第三方依赖：内置 cmdlet + 系统自带 curl.exe（GitHub Actions windows runner 与本机都有）。

用法
  .\scripts\sync-gitee-release.ps1 -Tag v0.3.2 -GiteeToken <token>
  .\scripts\sync-gitee-release.ps1 -Tag v0.3.2 -AssetsDir target\release\bundle\nsis -GiteeToken <token>
  .\scripts\sync-gitee-release.ps1 -All -GiteeToken <token>
  .\scripts\sync-gitee-release.ps1 -Tag v0.4.0 -PruneOld -GiteeToken <token>          # CI 本次发布
  .\scripts\sync-gitee-release.ps1 -All -PruneOld -GiteeToken <token>                # 回填后只留最新
#>
param(
    [string]$Tag = "",
    [switch]$All,
    [string]$AssetsDir = "",
    [string]$GitHubRepo = "KrisShin/Bead",
    [string]$GiteeRepo = "KrisShin/Bead",
    # Gitee 账号名（HTTPS 推 tag 用），缺省取 GiteeRepo 的 owner
    [string]$GiteeUser = "",
    [string]$GitHubToken = "",
    [string]$GiteeToken = "",
    [string]$PublishedTag = "",
    # 清掉旧版本附件，只留一个版本可下载；默认关闭，CI 显式开启
    [switch]$PruneOld,
    # 手动指定清理时保留哪个 tag；缺省取 Gitee 上最高的 v* 版本
    [string]$KeepTag = "",
    # 跳过 refs 同步（默认会先把分支与 tag 推到 Gitee，建 release 才有所指向的提交）
    [switch]$NoRefSync,
    # 只推 tag 不推分支
    [switch]$NoBranchMirror,
    # Gitee 上有本地没见过的提交、被拒推送时用
    [switch]$ForceRefs,
    [int]$MaxAttempts = 4
)
$ErrorActionPreference = 'Stop'

$script:GitHubApi = 'https://api.github.com'
$script:GiteeApi = 'https://gitee.com/api/v5'

if (-not $GitHubToken) { $GitHubToken = $env:GITHUB_TOKEN }
if (-not $GiteeToken) { $GiteeToken = $env:GITEE_TOKEN }
$script:GitHubToken = $GitHubToken
$script:GiteeToken = $GiteeToken
if (-not $GiteeUser) { $GiteeUser = ($GiteeRepo -split '/')[0] }
$script:GiteeUser = $GiteeUser
if ($MaxAttempts -lt 1) { $MaxAttempts = 4 }
$script:MaxAttempts = $MaxAttempts

if (-not $GiteeToken) {
    throw '缺少 GiteeToken：CI 需在仓库 Settings → Secrets 加 GITEE_TOKEN；本地用 -GiteeToken 传入（Gitee 私人令牌，勾 projects 权限）'
}
if (-not $Tag -and -not $All) {
    throw '用法：-Tag <vX.Y.Z> 同步单个版本，或 -All 回填全部历史版本'
}
if ($AssetsDir) {
    if (-not (Test-Path -LiteralPath $AssetsDir)) { throw "AssetsDir 不存在：$AssetsDir" }
    $AssetsDir = (Resolve-Path -LiteralPath $AssetsDir).Path
}

# Gitee 的 token 走查询串，报错信息与 CI 日志里必须抹掉，否则一次失败就把令牌打进公开日志；
# git 的 HTTPS 远程串里也有令牌（https://user:token@gitee.com/...），一起抹
function Hide-Secret {
    param([string]$Text)
    if (-not $Text) { return $Text }
    $Text = [regex]::Replace($Text, 'access_token=[^&\s"''<>)]+', 'access_token=***')
    $Text = [regex]::Replace($Text, '(?<=://)[^/@\s]+:[^/@\s]+@', '***:***@')
    return $Text
}

# Gitee 建 release 的前提是 commit 与 tag 都已经在那边。两个平台的代码是手工/延时同步的，
# 不能指望它先到——实测 Gitee main 落后一个版本时，POST /releases 直接 400「创建标签失败」。
# 所以顺序固定为「先同步 refs（分支 + 本次 tag），再建 release」，不存在 tag 指向远端没有的提交。
# 用私人令牌作 HTTPS 密码；幂等——远端已一致时 git 返回 up-to-date。
function Sync-GiteeRefs {
    param(
        [string[]]$Tags = @(),
        [switch]$NoBranches,
        [switch]$Force
    )
    $pushUrl = "https://$($script:GiteeUser):$($script:GiteeToken)@gitee.com/$GiteeRepo.git"

    # CI 的 checkout 只拉一个 ref，这里显式补齐全部分支与 tag，否则没有可推的引用
    Write-Host "  拉取全部分支与 tag ..." -ForegroundColor DarkGray
    $fetch = & git -c credential.helper= fetch --prune origin '+refs/heads/*:refs/remotes/origin/*' '+refs/tags/*:refs/tags/*' 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw ('拉取 origin 引用失败：' + [Environment]::NewLine + (Hide-Secret (($fetch | Out-String).Trim())))
    }

    $refspecs = @()
    if (-not $NoBranches) { $refspecs += 'refs/remotes/origin/*:refs/heads/*' }
    foreach ($t in $Tags) { $refspecs += "refs/tags/${t}:refs/tags/${t}" }
    if ($refspecs.Count -eq 0) { return }

    $label = if ($NoBranches) { "tag $($Tags -join ', ')" } else { "分支与 tag $($Tags -join ', ')" }
    Write-Host "  同步 $label 到 Gitee ..." -ForegroundColor DarkGray
    $gitArgs = @('-c', 'credential.helper=', 'push', '--no-verify')
    if ($Force) { $gitArgs += '--force' }
    $gitArgs += $pushUrl
    $gitArgs += $refspecs
    $out = & git @gitArgs 2>&1
    $code = $LASTEXITCODE
    $text = Hide-Secret (($out | Out-String).Trim())
    if ($code -ne 0) {
        throw ("同步 $label 到 Gitee 失败（git exit $code）" + [Environment]::NewLine + $text + [Environment]::NewLine +
            '排查：令牌需 projects 权限；CI 需 fetch-depth: 0（浅克隆缺历史对象）；' +
            'Gitee 上有本地没见过的提交时会拒绝推送，确认没有就加 -ForceRefs 覆盖')
    }
    if ($text) { Write-Host "    $text" -ForegroundColor DarkGray }
}

# ─────────────────────────────────────────────
# HTTP 工具：404 返回 $null（当作「不存在」），其余错误抛异常
# ─────────────────────────────────────────────
function Invoke-Api {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [string]$Method = 'GET',
        [hashtable]$Headers = @{},
        [byte[]]$Body = $null,
        [string]$ContentType = 'application/json',
        [int]$MaxAttempts = 1,
        [int]$RetryDelaySeconds = 3
    )
    $h = @{ 'User-Agent' = 'Bead-release-sync'; 'Connection' = 'close' }
    foreach ($k in $Headers.Keys) { $h[$k] = $Headers[$k] }
    if ($MaxAttempts -lt 1) { $MaxAttempts = 1 }

    $attempt = 0
    while ($attempt -lt $MaxAttempts) {
        $attempt++
        try {
            $p = @{ Uri = $Uri; Method = $Method; Headers = $h; UseBasicParsing = $true; TimeoutSec = 300 }
            if ($null -ne $Body -and $Body.Length -gt 0) {
                $p.Body = $Body
                if ($ContentType) { $p.ContentType = $ContentType }
            }
            $r = Invoke-WebRequest @p
            if (-not $r.Content) { return $null }
            return ($r.Content | ConvertFrom-Json)
        } catch {
            $status = $null
            $resp = $_.Exception.Response
            if ($resp -and $resp.StatusCode) {
                try { $status = [int]$resp.StatusCode } catch { $status = $null }
            }
            if ($null -eq $status -and $_.Exception.StatusCode) {
                try { $status = [int]$_.Exception.StatusCode } catch { $status = $null }
            }
            if ($status -eq 404) { return $null }

            $detail = ''
            if ($_.ErrorDetails.Message) { $detail = $_.ErrorDetails.Message }
            $retryable = ($null -eq $status) -or ($status -in @(429, 500, 502, 503, 504))
            if ($retryable -and $attempt -lt $MaxAttempts) {
                $delay = [int]($RetryDelaySeconds * [Math]::Pow(2, $attempt - 1))
                Write-Host "    [HTTP] $Method $(Hide-Secret $Uri) 第 $attempt/$MaxAttempts 次失败，$($delay)s 后重试：$($_.Exception.Message)" -ForegroundColor Yellow
                Start-Sleep -Seconds $delay
                continue
            }
            throw (Hide-Secret ("HTTP $status $Method $Uri (attempt $attempt/$MaxAttempts)" + [Environment]::NewLine + $detail))
        }
    }
    return $null
}

function ConvertTo-FormBody {
    param([hashtable]$Data)
    $pairs = foreach ($k in $Data.Keys) {
        $v = $Data[$k]
        if ($v -is [bool]) { $v = if ($v) { 'true' } else { 'false' } }
        "$([uri]::EscapeDataString([string]$k))=$([uri]::EscapeDataString([string]$v))"
    }
    return (($pairs -join '&'))
}

# Gitee v5：access_token 走表单/查询串，没有 Bearer 头
function Invoke-Gitee {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string]$Method = 'GET',
        [hashtable]$Form = @{},
        [int]$MaxAttempts = 0
    )
    $data = @{ access_token = $script:GiteeToken }
    foreach ($k in $Form.Keys) { $data[$k] = $Form[$k] }
    $attempts = if ($MaxAttempts -gt 0) { $MaxAttempts } elseif ($Method -in @('GET', 'PATCH', 'DELETE')) { 3 } else { 2 }

    # GET / DELETE 走查询串：DELETE 带 body 时部分网关会丢弃表单，access_token 随之丢失变成 401
    if ($Method -in @('GET', 'DELETE')) {
        return Invoke-Api -Uri ($script:GiteeApi + $Path + '?' + (ConvertTo-FormBody $data)) -Method $Method -MaxAttempts $attempts
    }
    $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-FormBody $data))
    return Invoke-Api -Uri ($script:GiteeApi + $Path) -Method $Method -Body $bytes -ContentType 'application/x-www-form-urlencoded' -MaxAttempts $attempts
}

function Invoke-GitHub {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [hashtable]$Query = @{},
        [int]$MaxAttempts = 3
    )
    $uri = $script:GitHubApi + $Path
    if ($Query.Count -gt 0) { $uri += '?' + (ConvertTo-FormBody $Query) }
    $headers = @{ Accept = 'application/vnd.github+json' }
    if ($script:GitHubToken) { $headers['Authorization'] = "Bearer $($script:GitHubToken)" }
    return Invoke-Api -Uri $uri -Headers $headers -MaxAttempts $MaxAttempts
}
# ─────────────────────────────────────────────
# GitHub 侧：读 Release、解析 tag 指向的提交、收集资产
# ─────────────────────────────────────────────
function Get-GitHubRelease {
    param([string]$TagName)
    return Invoke-GitHub -Path "/repos/$GitHubRepo/releases/tags/$TagName"
}

function Get-GitHubReleases {
    $all = @()
    $page = 1
    while ($true) {
        $items = @(Invoke-GitHub -Path "/repos/$GitHubRepo/releases" -Query @{ per_page = 100; page = $page })
        $all += $items
        if ($items.Count -lt 100) { break }
        $page++
    }
    return $all
}

# tag → commit SHA。annotated tag 要再剥一层；解析不出来退回 release 的 target_commitish
function Resolve-TagCommit {
    param([string]$TagName, [object]$Release)
    try {
        $ref = Invoke-GitHub -Path "/repos/$GitHubRepo/git/ref/tags/$TagName" -MaxAttempts 2
        if ($ref -and $ref.object) {
            if ($ref.object.type -eq 'tag' -and $ref.object.sha) {
                $tagObj = Invoke-GitHub -Path "/repos/$GitHubRepo/git/tags/$($ref.object.sha)" -MaxAttempts 2
                if ($tagObj -and $tagObj.object -and $tagObj.object.sha) { return [string]$tagObj.object.sha }
            }
            if ($ref.object.sha) { return [string]$ref.object.sha }
        }
    } catch {
        Write-Host "    解析 tag 提交失败，退回 target_commitish：$($_.Exception.Message)" -ForegroundColor Yellow
    }
    if ($Release -and $Release.target_commitish) { return [string]$Release.target_commitish }
    return 'main'
}

# 资产白名单：安装包与校验文件同步，构建中间物不同步
function Test-SyncableAsset {
    param([string]$Name)
    if (-not $Name) { return $false }
    if ($Name -match '\.(pdb|wixpdb|wxs|sig|map)$') { return $false }
    if ($Name -match '-debug') { return $false }
    return $true
}

function Save-GitHubAssets {
    param([object]$Release, [string]$WorkDir)
    $map = @{}
    foreach ($a in @($Release.assets)) {
        if (-not (Test-SyncableAsset $a.name)) { continue }
        $out = Join-Path $WorkDir $a.name
        Write-Host "  下载 $($a.name) ..." -ForegroundColor DarkGray
        $ok = $false
        $h = @{ 'User-Agent' = 'Bead-release-sync' }
        if ($script:GitHubToken) { $h['Authorization'] = "Bearer $($script:GitHubToken)" }
        # 公开仓库直接走 browser_download_url：免 token、curl 跟 302 到 CDN
        try {
            Invoke-WebRequest -Uri $a.browser_download_url -Headers $h -OutFile $out -TimeoutSec 600 -UseBasicParsing
            $ok = Test-Path -LiteralPath $out
        } catch { $ok = $false }
        if (-not $ok -and $a.url -and $script:GitHubToken) {
            $h2 = @{ 'User-Agent' = 'Bead-release-sync'; 'Accept' = 'application/octet-stream'; 'Authorization' = "Bearer $($script:GitHubToken)" }
            Invoke-WebRequest -Uri $a.url -Headers $h2 -OutFile $out -TimeoutSec 600 -UseBasicParsing
            $ok = Test-Path -LiteralPath $out
        }
        if (-not $ok) { throw "下载 GitHub 资产失败：$($a.name)" }
        $map[$a.name] = $out
    }
    return $map
}

# 本地产物直传（-AssetsDir）：CI 与本地都能用，不经过 GitHub 下载
function Get-LocalAssets {
    param([string]$Dir)
    $map = @{}
    foreach ($f in (Get-ChildItem -LiteralPath $Dir -File)) {
        if (-not (Test-SyncableAsset $f.Name)) { continue }
        $map[$f.Name] = $f.FullName
    }
    return $map
}
# ─────────────────────────────────────────────
# Gitee 侧：附件列表解析（字段名有变体，容器可能再包一层）
# ─────────────────────────────────────────────
function Get-AttachmentName {
    param([object]$F)
    if ($null -eq $F) { return '' }
    foreach ($k in @('name', 'filename')) {
        $v = $F.$k
        if ($v -is [string] -and $v.Length -gt 0) { return $v }
    }
    if ($F.browser_download_url -is [string] -and $F.browser_download_url.Length -gt 0) {
        try { return [IO.Path]::GetFileName([string]$F.browser_download_url) } catch { }
    }
    return ''
}

function Get-AttachmentList {
    param([object]$Data)
    if ($null -eq $Data) { return @() }
    if ($Data -is [System.Array]) { return @($Data) }
    foreach ($k in @('assets', 'attachments', 'data', 'rows', 'list', 'content', 'files')) {
        $v = $Data.$k
        if ($null -ne $v -and $v -is [System.Array]) { return @($v) }
    }
    return @($Data)
}

function Get-GiteeAttachments {
    param([string]$ReleaseId)
    # release 对象内嵌的 assets 不带附件 id（还混入平台自动生成的源码包），增删必须以本接口为准
    $files = Invoke-Gitee -Path "/repos/$GiteeRepo/releases/$ReleaseId/attach_files" -Form @{ page = 1; per_page = 100 }
    return @(Get-AttachmentList $files)
}

function Remove-GiteeAttachment {
    param([string]$ReleaseId, [string]$AttachmentId, [string]$Name)
    if (-not $AttachmentId) { return }
    $null = Invoke-Gitee -Path "/repos/$GiteeRepo/releases/$ReleaseId/attach_files/$AttachmentId" -Method 'DELETE'
    Write-Host "    删除附件 $Name" -ForegroundColor Yellow
}

# 创建 release：Gitee 的 POST 偶发超时但服务端已建成，先回查再决定是否重试
function New-GiteeRelease {
    param([string]$TagName, [hashtable]$Form)
    $lastError = $null
    for ($attempt = 1; $attempt -le $script:MaxAttempts; $attempt++) {
        try {
            $rel = Invoke-Gitee -Path "/repos/$GiteeRepo/releases" -Method 'POST' -Form $Form -MaxAttempts 1
            if ($rel) { return $rel }
        } catch {
            $lastError = $_
            $msg = [string]$_.Exception.Message
            if ($msg -match 'HTTP (400|401|403|404|405|422) ') { throw $msg }
        }
        try {
            $existing = Invoke-Gitee -Path "/repos/$GiteeRepo/releases/tags/$TagName" -MaxAttempts 2
            if ($existing) {
                Write-Host "    已确认 release 实际创建成功，继续同步附件" -ForegroundColor DarkGray
                return $existing
            }
        } catch { }
        if ($attempt -lt $script:MaxAttempts) {
            $delay = [int](3 * [Math]::Pow(2, $attempt - 1))
            Write-Host "    创建 release 失败，$($delay)s 后回查并重试（$attempt/$($script:MaxAttempts)）" -ForegroundColor Yellow
            Start-Sleep -Seconds $delay
        }
    }
    throw ("创建 Gitee release 失败：$TagName（已试 $($script:MaxAttempts) 次）" + [Environment]::NewLine + $lastError)
}

# 单文件上传：curl 写状态码到 stdout、响应体到临时文件，便于区分「半包」与「鉴权失败」
function Send-GiteeAttachment {
    param([string]$ReleaseId, [string]$Name, [string]$File)
    $url = "$($script:GiteeApi)/repos/$GiteeRepo/releases/$ReleaseId/attach_files"
    $respFile = [IO.Path]::GetTempFileName()
    try {
        $curlArgs = @(
            '-sS', '--http1.1', '--connect-timeout', '20', '--max-time', '1800',
            '--speed-time', '60', '--speed-limit', '1024',
            '-X', 'POST',
            '-F', "access_token=$($script:GiteeToken)",
            '-F', "file=@$File;filename=$Name",
            '-w', '%{http_code}',
            '-o', $respFile,
            $url
        )
        $raw = (& curl.exe @curlArgs 2>&1) -join ' '
        $status = 0
        if ($raw -match '(\d{3})\s*$') { $status = [int]$Matches[1] }
        $detail = ''
        if (Test-Path -LiteralPath $respFile) { $detail = (Get-Content -LiteralPath $respFile -Raw) }
        if ($detail -and $detail.Length -gt 400) { $detail = $detail.Substring(0, 400) }
        return @{ Status = $status; ExitCode = $LASTEXITCODE; Detail = $detail }
    } finally {
        Remove-Item -LiteralPath $respFile -Force -ErrorAction SilentlyContinue
    }
}

# ─────────────────────────────────────────────
# 旧版本清理：只留最新版可下载
# ─────────────────────────────────────────────
function ConvertTo-VersionOrZero {
    param([string]$Text)
    try { return [version]($Text -replace '^[vV]', '') } catch { return [version]'0.0.0' }
}

function Get-GiteeReleases {
    $all = @()
    $page = 1
    while ($true) {
        $items = @(Invoke-Gitee -Path "/repos/$GiteeRepo/releases" -Form @{ page = $page; per_page = 100 })
        $all += $items
        if ($items.Count -lt 100) { break }
        $page++
    }
    return $all
}

# 保留哪个版本：显式 -KeepTag 优先，否则取「最高的、且确实带附件的 v*」。
# 两点刻意：不取「本次发布的 tag」——给老分支发补丁版时那会把主线最新版误清；
# 跳过没附件的高版本——只有更新说明的 release 不该让整轮发布报错。
function Resolve-KeepRelease {
    param([string]$Preferred)
    $cands = @(Get-GiteeReleases |
        Where-Object { ([string]$_.tag_name) -match '^[vV]\d+\.\d+\.\d+$' } |
        Sort-Object { ConvertTo-VersionOrZero ([string]$_.tag_name) } -Descending)
    if ($cands.Count -eq 0) { return $null }
    if ($Preferred) {
        $hit = $cands | Where-Object { ([string]$_.tag_name) -eq $Preferred } | Select-Object -First 1
        if (-not $hit) { throw "拒绝清理：Gitee 上没有 $Preferred" }
        return $hit
    }
    foreach ($c in $cands) {
        $files = @(Get-GiteeAttachments -ReleaseId ([string]$c.id))
        if ($files.Count -gt 0) { return $c }
    }
    return $null
}

function Invoke-PruneOldReleases {
    param([object]$KeepRelease)

    if (-not $KeepRelease) { throw '拒绝清理：Gitee 上没有带附件的 v* release' }
    $keepTag = [string]$KeepRelease.tag_name

    # 前置闸门：保留版本必须真有附件。顺序反了就会出现「旧的全删了、新的还没传上」
    $keepFiles = @(Get-GiteeAttachments -ReleaseId ([string]$KeepRelease.id))
    if ($keepFiles.Count -eq 0) { throw "拒绝清理：$keepTag 没有任何附件，清掉旧版本会让所有版本都下不到安装包" }

    $all = @(Get-GiteeReleases)
    $pruned = 0
    $keptTags = @()
    foreach ($r in $all) {
        $t = [string]$r.tag_name
        if ($t -eq $keepTag) {
            Write-Host "  [保留] $t（$($keepFiles.Count) 个附件）" -ForegroundColor Green
            continue
        }
        # 只清软件版本的安装包：ocr-v* 是已装用户的模型备源，清掉等于削减线上可用性
        if ($t -notmatch '^[vV]\d+\.\d+\.\d+$') {
            Write-Host "  [不动] $t（不是软件版本，附件另有用途）" -ForegroundColor DarkGray
            $keptTags += $t
            continue
        }
        $files = @(Get-GiteeAttachments -ReleaseId ([string]$r.id))
        if ($files.Count -eq 0) { continue }
        foreach ($f in $files) {
            Remove-GiteeAttachment -ReleaseId ([string]$r.id) -AttachmentId ([string]$f.id) -Name "$t / $(Get-AttachmentName $f)"
            $pruned++
        }
    }
    Write-Host "  已清掉 $pruned 个旧附件（$keepTag 之外的 release 只保留 tag 与更新说明）" -ForegroundColor Yellow
    if ($keptTags.Count -gt 0) { Write-Host "  未涉及的 release：$($keptTags -join ', ')" -ForegroundColor DarkGray }
    return $keepTag
}

function Sync-GiteeRelease {
    param(
        [string]$TagName,
        [string]$Name,
        [string]$Body,
        [bool]$Prerelease,
        [string]$Commitish,
        [hashtable]$AssetMap,
        [switch]$Reconcile
    )

    $rel = Invoke-Gitee -Path "/repos/$GiteeRepo/releases/tags/$TagName"
    if ($rel -and -not $Reconcile) {
        Write-Host "  [跳过] Gitee 已有 $TagName（历史版本不改写）" -ForegroundColor DarkGray
        return 'skipped'
    }

    if (-not $rel) {
        Write-Host "  [创建] Gitee release $TagName ..." -ForegroundColor Green
        $form = @{
            tag_name         = $TagName
            name             = $Name
            body             = $Body
            target_commitish = $Commitish
            prerelease       = $Prerelease
        }
        $rel = New-GiteeRelease -TagName $TagName -Form $form
    } else {
        Write-Host "  [更新] Gitee release $TagName 的标题与正文 ..." -ForegroundColor Green
        $null = Invoke-Gitee -Path "/repos/$GiteeRepo/releases/$($rel.id)" -Method 'PATCH' -Form @{
            tag_name = $TagName; name = $Name; body = $Body; prerelease = $Prerelease
        }
    }
    if (-not $rel -or -not $rel.id) { throw "Gitee release 对象缺少 id：$TagName" }

    $releaseId = [string]$rel.id
    $remote = @(Get-GiteeAttachments -ReleaseId $releaseId)
    $existing = @{}
    foreach ($f in $remote) {
        $fn = Get-AttachmentName $f
        if ($fn) {
            if ($null -ne $f.size) { $existing[$fn] = [int64]$f.size } else { $existing[$fn] = [int64]-1 }
        }
    }

    $todo = @()
    foreach ($assetName in @($AssetMap.Keys)) {
        $file = $AssetMap[$assetName]
        $size = [int64](Get-Item -LiteralPath $file).Length
        if ($existing.ContainsKey($assetName) -and $existing[$assetName] -eq $size) {
            Write-Host "    已存在且大小一致：$assetName" -ForegroundColor DarkGray
            continue
        }
        if ($existing.ContainsKey($assetName)) {
            $old = $remote | Where-Object { (Get-AttachmentName $_) -eq $assetName } | Select-Object -First 1
            if ($old) { Remove-GiteeAttachment -ReleaseId $releaseId -AttachmentId ([string]$old.id) -Name $assetName }
        }
        $todo += @{ Name = $assetName; File = $file; Size = $size }
    }

    if ($todo.Count -eq 0) {
        Write-Host "  附件已齐，无需上传" -ForegroundColor DarkGray
        return 'unchanged'
    }

    Write-Host "  [上传] $($todo.Count) 个附件 ..." -ForegroundColor Cyan
    $failed = @()
    foreach ($item in $todo) {
        $ok = $false
        $lastDetail = ''
        for ($attempt = 1; $attempt -le $script:MaxAttempts; $attempt++) {
            $r = Send-GiteeAttachment -ReleaseId $releaseId -Name $item.Name -File $item.File
            if ($r.Status -in @(401, 403, 422)) {
                throw "Gitee 附件上传被拒：$($item.Name) HTTP $($r.Status)$([Environment]::NewLine)$($r.Detail)"
            }
            if ($r.Status -ge 200 -and $r.Status -lt 300) {
                $now = @(Get-GiteeAttachments -ReleaseId $releaseId)
                $match = $now | Where-Object { (Get-AttachmentName $_) -eq $item.Name } | Select-Object -First 1
                if ($match -and [int64]$match.size -eq $item.Size) {
                    Write-Host ("    OK  {0}（{1} MB）" -f $item.Name, [math]::Round($item.Size / 1MB, 1)) -ForegroundColor Green
                    $ok = $true
                    break
                }
                if ($match) {
                    Write-Host "    上传后大小不符（远端 $($match.size) / 本地 $($item.Size)），重传" -ForegroundColor Yellow
                    Remove-GiteeAttachment -ReleaseId $releaseId -AttachmentId ([string]$match.id) -Name $item.Name
                }
            }
            $lastDetail = "http=$($r.Status) exit=$($r.ExitCode) $($r.Detail)"
            if ($attempt -lt $script:MaxAttempts) {
                $delay = [int](5 * [Math]::Pow(2, $attempt - 1))
                Write-Host "    重试 $($item.Name)（$attempt/$($script:MaxAttempts)），$($delay)s 后" -ForegroundColor Yellow
                Start-Sleep -Seconds $delay
            }
        }
        if (-not $ok) {
            $failed += $item.Name
            Write-Host "    FAIL $($item.Name)：$lastDetail" -ForegroundColor Red
        }
    }

    if ($failed.Count -gt 0) {
        throw "Gitee 附件上传失败 $($failed.Count) 个：$($failed -join ', ')（可重跑，已完整的附件会跳过）"
    }
    return 'synced'
}

# ─────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────
$work = Join-Path $env:TEMP ("bead-gitee-sync-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work -Force | Out-Null

function Sync-OneVersion {
    param([string]$TagName, [object]$Release, [switch]$Reconcile, [string]$WorkDir)
    $name = if ($Release -and $Release.name) { [string]$Release.name } else { "Bead $TagName" }
    $body = if ($Release -and $Release.body) { [string]$Release.body } else { "Bead $TagName" }
    $pre = [bool]($Release -and $Release.prerelease)
    $commit = Resolve-TagCommit -TagName $TagName -Release $Release
    $assets = if ($AssetsDir) { Get-LocalAssets -Dir $AssetsDir } else { Save-GitHubAssets -Release $Release -WorkDir $WorkDir }
    if ($assets.Count -eq 0) {
        # 只有说明没附件的历史 release 也要镜像页面；但 CI 传了 AssetsDir 却是空目录，属于构建产出异常，必须红
        if ($AssetsDir) { throw "AssetsDir 里没有可同步的资产：$AssetsDir" }
        Write-Host "  该 release 没有附件，只镜像 release 页" -ForegroundColor Yellow
    }
    Write-Host "`n== $TagName ==" -ForegroundColor Cyan
    return (Sync-GiteeRelease -TagName $TagName -Name $name -Body $body -Prerelease $pre -Commitish $commit -AssetMap $assets -Reconcile:$Reconcile)
}

$summary = @()
$failures = 0
try {
    # 先同步 refs 再同步 release：Gitee 建 release 要求 tag 指向的提交已存在，
    # 顺序反了就是 400「创建标签失败」。分支一起推，Gitee 仓库的源码也顺带追平。
    if (-not $NoRefSync) {
        $refTags = if ($Tag) {
            @($Tag)
        } else {
            @(Get-GitHubReleases | Where-Object { -not $_.draft } | ForEach-Object { [string]$_.tag_name })
        }
        Sync-GiteeRefs -Tags $refTags -NoBranches:$NoBranchMirror -Force:$ForceRefs
    }

    if ($Tag) {
        $rel = Get-GitHubRelease -TagName $Tag
        if (-not $rel -and -not $AssetsDir) { throw "GitHub 上没有 release：$Tag" }
        # 显式指定单版本 = 明确要它同步到位，因此允许补齐缺失附件
        $state = Sync-OneVersion -TagName $Tag -Release $rel -Reconcile -WorkDir $work
        $summary += "$Tag : $state"
    } else {
        Write-Host "枚举 $GitHubRepo 的全部 release ..." -ForegroundColor Cyan
        $releases = @(Get-GitHubReleases | Where-Object { -not $_.draft })
        if ($releases.Count -eq 0) {
            Write-Host "GitHub 上没有可同步的 release" -ForegroundColor Yellow
            exit 0
        }
        $sorted = @($releases | Sort-Object { ConvertTo-VersionOrZero ([string]$_.tag_name) } -Descending)
        foreach ($r in $sorted) {
            $t = [string]$r.tag_name
            $reconcile = [bool]($PublishedTag -and $t -eq $PublishedTag)
            try {
                $state = Sync-OneVersion -TagName $t -Release $r -Reconcile:$reconcile -WorkDir $work
                $summary += "$t : $state"
            } catch {
                $failures++
                $summary += "$t : FAILED"
                Write-Host "  [!] $t 同步失败：$($_.Exception.Message)" -ForegroundColor Red
                if ($reconcile) { throw }
            } finally {
                # 每个版本清一次临时目录：-All 回填不该把历史安装包全堆在 runner 上
                Get-ChildItem -LiteralPath $work -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
            }
        }
    }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

# 清理放在同步之后：有失败就不清——新版没传全就把旧版删了，等于把人逼到没得下载
if ($PruneOld) {
    if ($failures -gt 0) {
        Write-Host "`n本次有 $failures 个版本同步失败，跳过旧版本清理" -ForegroundColor Yellow
    } else {
        Write-Host "`n=== 清理旧版本附件 ===" -ForegroundColor Cyan
        $keepRel = Resolve-KeepRelease -Preferred $KeepTag
        Write-Host "  保留版本：$([string]$keepRel.tag_name)" -ForegroundColor Cyan
        $kept = Invoke-PruneOldReleases -KeepRelease $keepRel
        $summary += "prune : 只保留 $kept"
    }
}

Write-Host "`n=== 同步结果 ===" -ForegroundColor Cyan
$summary | ForEach-Object { Write-Host "  $_" }
Write-Host "Gitee: https://gitee.com/$GiteeRepo/releases" -ForegroundColor Green
if ($failures -gt 0) {
    Write-Host "有 $failures 个版本失败，见上方日志" -ForegroundColor Yellow
    exit 1
}
Write-Host "=== sync done ===" -ForegroundColor Green



