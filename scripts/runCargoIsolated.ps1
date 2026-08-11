param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("test", "check", "clippy")]
  [string]$CargoCommand,
  [string]$PackageFilter = "",
  [switch]$Workspace,
  [switch]$AllTargets,
  [switch]$AllFeatures,
  [switch]$DenyWarnings
)

# 在系统临时目录中执行 Cargo 命令，避免检查、测试和 Clippy 把缓存留在工作区 target/。
$temporaryTargetName = "capture-cargo-" + [System.Guid]::NewGuid().ToString("N")
$temporaryTargetDirectory = Join-Path ([System.IO.Path]::GetTempPath()) $temporaryTargetName
$previousTargetDirectory = [System.Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR", "Process")
$cargoArguments = @($CargoCommand)
if ($Workspace) {
  if (-not [string]::IsNullOrWhiteSpace($PackageFilter)) {
    throw "--workspace 不能与 PackageFilter 同时使用。"
  }
  $cargoArguments += "--workspace"
} elseif (-not [string]::IsNullOrWhiteSpace($PackageFilter)) {
  foreach ($packageName in $PackageFilter.Split(",", [System.StringSplitOptions]::RemoveEmptyEntries)) {
    $cargoArguments += "-p"
    $cargoArguments += $packageName.Trim()
  }
}
if ($AllTargets) {
  $cargoArguments += "--all-targets"
}
if ($AllFeatures) {
  $cargoArguments += "--all-features"
}
if ($DenyWarnings) {
  $cargoArguments += "--"
  $cargoArguments += "-D"
  $cargoArguments += "warnings"
}

$commandExitCode = 1
$cleanupExitCode = 0
try {
  $env:CARGO_TARGET_DIR = $temporaryTargetDirectory
  & cargo @cargoArguments
  $commandExitCode = $LASTEXITCODE
} finally {
  # Cargo clean 是唯一的清理通道，确保依赖缓存和增量对象都随任务专属目录一并销毁。
  & cargo clean --target-dir $temporaryTargetDirectory
  $cleanupExitCode = $LASTEXITCODE
  if (Test-Path -LiteralPath $temporaryTargetDirectory) {
    throw "Cargo 临时构建目录未清理：$temporaryTargetDirectory"
  }
  if ($null -eq $previousTargetDirectory) {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
  } else {
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
  }
}

if ($cleanupExitCode -ne 0) {
  exit $cleanupExitCode
}
exit $commandExitCode
