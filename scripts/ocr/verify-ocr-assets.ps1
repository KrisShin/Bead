#requires -Version 7.0
<#
.SYNOPSIS
    校验 ocr-manifest.json 里每个文件的每一个下载源：可达、长度正确、并顺带量 RTT 与采样吞吐。

.DESCRIPTION
    骨架脚本，可用于发布后自检，也可当作应用内「测速策略」的参考实现。
    用法：
        ./verify-ocr-assets.ps1 -ManifestPath ./models/ocr-manifest.json
        ./verify-ocr-assets.ps1 -ManifestPath ./models/ocr-manifest.json -SampleKB 256

    输出四列：RTT（毫秒）、采样吞吐（KB/s）、Content-Range 总长、结论。
    任一源失败时以 exit 1 结束，方便挂到 CI。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath,

    [int]$SampleKB = 64,
    [int]$TimeoutSec = 25
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $ManifestPath)) { throw "清单不存在：$ManifestPath" }
$manifest = Get-Content $ManifestPath -Raw | ConvertFrom-Json

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromSeconds($TimeoutSec)

function Invoke-RangeProbe {
    param([string]$Url, [int64]$Bytes)

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, $Url)
    $request.Headers.Range = [System.Net.Http.Headers.RangeHeaderValue]::new(0, ($Bytes - 1))
    $response = $client.SendAsync($request, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).Result
    $stream = $response.Content.ReadAsStreamAsync().Result
    $buffer = [byte[]]::new(8192)
    $read = 0L
    while (($n = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) { $read += $n }
    $sw.Stop()
    $stream.Dispose()

    $contentRange = if ($response.Content.Headers.Contains('Content-Range')) {
        $response.Content.Headers.GetValues('Content-Range') | Select-Object -First 1
    } else { '' }
    $total = if ($contentRange -match '/(\d+)$') { [int64]$Matches[1] } else { $read }
    $statusCode = [int]$response.StatusCode
    $response.Dispose()

    [pscustomobject]@{
        Status       = $statusCode
        Bytes        = $read
        Total        = $total
        Milliseconds = [int]$sw.ElapsedMilliseconds
        KBps         = if ($sw.ElapsedMilliseconds -gt 0) { [math]::Round($read / 1KB / ($sw.ElapsedMilliseconds / 1000.0), 1) } else { 0 }
        ContentRange = $contentRange
    }
}

$failures = 0
$rows = foreach ($file in $manifest.files) {
    foreach ($url in $file.sources) {
        if ($url -match 'YOUR_GITEE_OWNER') {
            [pscustomobject]@{ File = $file.name; Source = 'gitee'; RTTms = '-'; KBps = '-'; Total = '-'; Result = 'SKIP（占位符 URL）' }
            continue
        }
        $hostName = ([uri]$url).Host
        try {
            $oneByte = Invoke-RangeProbe -Url $url -Bytes 1
            $sample = Invoke-RangeProbe -Url $url -Bytes ([int64]$SampleKB * 1KB)
            $ok = ($sample.Total -eq [int64]$file.size) -and ($sample.Status -eq 206 -or $sample.Bytes -eq [int64]$file.size)
            if (-not $ok) { $failures++ }
            [pscustomobject]@{
                File   = $file.name
                Source = $hostName
                RTTms  = $oneByte.Milliseconds
                KBps   = $sample.KBps
                Total  = $sample.Total
                Result = if ($ok) { 'OK' } else { "FAIL（期望 $($file.size)）" }
            }
        } catch {
            $failures++
            [pscustomobject]@{ File = $file.name; Source = $hostName; RTTms = '-'; KBps = '-'; Total = '-'; Result = "FAIL：$($_.Exception.Message)" }
        }
    }
}

$rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
Write-Host ("清单版本 {0}，共 {1} 个文件，失败 {2} 项。" -f $manifest.version, @($manifest.files).Count, $failures)

if ($failures -gt 0) { exit 1 }
