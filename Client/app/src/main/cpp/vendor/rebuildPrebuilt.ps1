param(
    [Parameter(Mandatory = $true)]
    [string]$NdkRoot
)

$ErrorActionPreference = "Stop"
$vendorRoot = $PSScriptRoot
$repositoryRoot = (Resolve-Path (Join-Path $vendorRoot "..\..\..\..\..\..")).Path
$workRoot = Join-Path $repositoryRoot "tmp\nativeVendorRebuild"
if ([IO.Path]::GetPathRoot($workRoot) -ne "D:\") {
    throw "依赖重建临时目录必须位于 D 盘"
}
$lock = Get-Content -Raw -Encoding UTF8 (Join-Path $vendorRoot "sourceLock.json") | ConvertFrom-Json
$toolRoot = Join-Path $NdkRoot "toolchains\llvm\prebuilt\windows-x86_64\bin"
$ndkBuild = Join-Path $NdkRoot "ndk-build.cmd"
$strip = Join-Path $toolRoot "llvm-strip.exe"
$ar = Join-Path $toolRoot "llvm-ar.exe"
$hevSource = Join-Path $workRoot "hev-socks5-tunnel"
$hevOutput = Join-Path $workRoot "hevOutput"
$monoOutput = Join-Path $workRoot "monocypherOutput"
$systemTemp = Join-Path $workRoot "systemTemp"
$originalTemp = $env:TEMP
$originalTmp = $env:TMP
$temporaryEnvironmentSet = $false

# Windows 默认不创建 Git 符号链接；把上游120000条目物化为目标内容后再交给 NDK。
function Expand-GitLinks([string]$sourceRoot) {
    $linkRows = & git -C $sourceRoot ls-files -s | Where-Object { $_ -match '^120000 ' }
    foreach ($row in $linkRows) {
        $relative = $row.Substring($row.IndexOf("`t") + 1)
        $linkPath = Join-Path $sourceRoot $relative
        if ((Get-Item -LiteralPath $linkPath).LinkType) { continue }
        # 从 Git 对象读取链接正文，避免 Windows 文件 API 把已物化链接当作目标文件展开。
        $targetText = ((& git -C $sourceRoot show ":$relative") | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $targetText.Contains("`n") -or $targetText.Contains("`r")) {
            throw "上游符号链接正文无效：$relative"
        }
        $targetPath = [IO.Path]::GetFullPath((Join-Path (Split-Path $linkPath) $targetText))
        $content = [IO.File]::ReadAllBytes($targetPath)
        [IO.File]::WriteAllBytes($linkPath, $content)
    }
}

$taskFailure = $null
try {
    if (Test-Path $workRoot) {
        $resolved = (Resolve-Path $workRoot).Path
        if ($resolved -ne $workRoot) { throw "临时目录解析结果异常" }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
    New-Item -ItemType Directory -Force $workRoot, $hevOutput, $monoOutput, $systemTemp | Out-Null
    # Git、Invoke-WebRequest 与 NDK 都继承任务专属 D 盘临时目录，避免调用者环境把缓存写入 C 盘。
    $env:TEMP = $systemTemp
    $env:TMP = $systemTemp
    $temporaryEnvironmentSet = $true
    & git clone --recursive $lock.hev.repository $hevSource
    if ($LASTEXITCODE -ne 0) { throw "HEV 源码获取失败" }
    & git -C $hevSource checkout $lock.hev.commit
    & git -C $hevSource submodule update --init --recursive
    if ($LASTEXITCODE -ne 0) { throw "HEV 固定提交检出失败" }
    foreach ($entry in $lock.hev.submodules.psobject.Properties) {
        $actual = (& git -C (Join-Path $hevSource $entry.Name) rev-parse HEAD).Trim()
        if ($actual -ne $entry.Value) { throw "HEV 子模块提交不匹配：$($entry.Name)" }
    }
    $patch = Join-Path $vendorRoot "hev-socks5-tunnel\patches\routesocks.patch"
    $patchHash = (Get-FileHash $patch -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($patchHash -ne $lock.hev.patchSha256) { throw "HEV 本地补丁哈希不匹配" }
    & git -C $hevSource apply --ignore-whitespace $patch
    if ($LASTEXITCODE -ne 0) { throw "HEV 本地补丁应用失败" }
    Expand-GitLinks $hevSource
    Expand-GitLinks (Join-Path $hevSource "src\core")
    Expand-GitLinks (Join-Path $hevSource "third-part\hev-task-system")
    Expand-GitLinks (Join-Path $hevSource "third-part\lwip")
    Expand-GitLinks (Join-Path $hevSource "third-part\yaml")

    $objectRoot = Join-Path $workRoot "hevObjects"
    $libraryRoot = Join-Path $workRoot "hevLibraries"
    & $ndkBuild "NDK_PROJECT_PATH=$hevSource" "APP_BUILD_SCRIPT=$hevSource\Android.mk" `
        "NDK_APPLICATION_MK=$hevSource\Application.mk" "NDK_OUT=$objectRoot" `
        "NDK_LIBS_OUT=$libraryRoot" "APP_ABI=arm64-v8a armeabi-v7a" -B -j8
    if ($LASTEXITCODE -ne 0) { throw "HEV 双 ABI 编译失败" }

    $monoArchive = Join-Path $workRoot "monocypher.tar.gz"
    Invoke-WebRequest $lock.monocypher.archiveUrl -OutFile $monoArchive
    $monoHash = (Get-FileHash $monoArchive -Algorithm SHA512).Hash.ToLowerInvariant()
    if ($monoHash -ne $lock.monocypher.archiveSha512) { throw "Monocypher 官方包哈希不匹配" }
    # Windows 无法创建上游文档中的冒号文件名；重建只提取已锁定版本的核心实现。
    tar -xf $monoArchive -C $workRoot `
        "monocypher-$($lock.monocypher.version)/src/monocypher.c" `
        "monocypher-$($lock.monocypher.version)/src/monocypher.h"
    if ($LASTEXITCODE -ne 0) { throw "Monocypher 核心源码提取失败" }
    $monoSource = Join-Path $workRoot "monocypher-$($lock.monocypher.version)\src\monocypher.c"

    $abis = @(
        @{ Name = "arm64-v8a"; Compiler = "aarch64-linux-android24-clang.cmd" },
        @{ Name = "armeabi-v7a"; Compiler = "armv7a-linux-androideabi24-clang.cmd" }
    )
    foreach ($abi in $abis) {
        $name = $abi.Name
        $compiler = Join-Path $toolRoot $abi.Compiler
        $combinedObject = Join-Path $hevOutput "$name\hevCombined.o"
        $hevArchive = Join-Path $hevOutput "$name\libhev-socks5-tunnel-static.a"
        $monoObject = Join-Path $monoOutput "$name\monocypher.o"
        $monoArchiveOut = Join-Path $monoOutput "$name\libmonocypher.a"
        New-Item -ItemType Directory -Force (Split-Path $combinedObject), (Split-Path $monoObject) | Out-Null
        $local = Join-Path $objectRoot "local\$name"
        & $compiler -r '-Wl,--whole-archive' (Join-Path $local "libhev-socks5-tunnel-static.a") `
            '-Wl,--no-whole-archive' '-Wl,--start-group' (Join-Path $local "libhev-task-system.a") `
            (Join-Path $local "liblwip.a") (Join-Path $local "libyaml.a") '-Wl,--end-group' -o $combinedObject
        if ($LASTEXITCODE -ne 0) { throw "HEV $name 归档合并失败" }
        & $strip --strip-debug $combinedObject
        & $ar rcs $hevArchive $combinedObject
        & $compiler -std=c11 -O2 -fPIC -fvisibility=hidden -Wall -Wextra -Werror -c $monoSource -o $monoObject
        if ($LASTEXITCODE -ne 0) { throw "Monocypher $name 编译失败" }
        & $strip --strip-debug $monoObject
        & $ar rcs $monoArchiveOut $monoObject
        Copy-Item $hevArchive (Join-Path $vendorRoot "hev-socks5-tunnel\prebuilt\$name\") -Force
        Copy-Item $monoArchiveOut (Join-Path $vendorRoot "monocypher\prebuilt\$name\") -Force
    }
    Write-Host "双 ABI 预构建依赖已从锁定来源重建；请复核 sourceLock.json 中的归档哈希。"
} catch {
    $taskFailure = $_
} finally {
    # 先恢复调用者环境，再删除任务目录，避免 PowerShell 后续命令引用已经清理的 TEMP。
    if ($temporaryEnvironmentSet) {
        $env:TEMP = $originalTemp
        $env:TMP = $originalTmp
    }
    if (Test-Path $workRoot) {
        for ($attempt = 0; $attempt -lt 3; ++$attempt) {
            try {
                Remove-Item -LiteralPath $workRoot -Recurse -Force
                break
            } catch {
                Start-Sleep -Milliseconds 200
            }
        }
        if ((Test-Path $workRoot) -and $null -eq $taskFailure) {
            throw "依赖重建临时目录清理失败：$workRoot"
        }
    }
}
if ($null -ne $taskFailure) { throw $taskFailure }
