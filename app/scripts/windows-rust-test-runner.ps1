param(
  [Parameter(Mandatory = $true, Position = 0)]
  [string]$TestExecutable,

  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$TestArguments = @()
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$executable = (Resolve-Path -LiteralPath $TestExecutable).Path
if ([System.IO.Path]::GetFileName($executable) -match '^video_for_lazies_lib-[0-9a-f]+\.exe$') {
  # build.rs attaches manifests to product binaries individually. Cargo's lib
  # unit-test executable has no product manifest, yet links the same GUI APIs.
  # Give only that generated harness the main app's Common Controls context.
  $item = Get-Item -LiteralPath $executable
  if ($item.LinkType -or $item.Directory.Name -ne "deps" -or
      $item.Directory.Parent.Name -notin @("debug", "release")) {
    throw "Refusing to modify an unexpected library test executable."
  }
  $manifest = (Resolve-Path (Join-Path $PSScriptRoot "..\src-tauri\video-for-lazies.manifest")).Path
  if ($env:GITHUB_ACTIONS -eq "true") {
    $diagnostics = Join-Path $env:RUNNER_TEMP "vfl-unit-test-loader"
    [void][System.IO.Directory]::CreateDirectory($diagnostics)
    Copy-Item -LiteralPath $executable -Destination (Join-Path $diagnostics $item.Name) -Force
  }
  $tool = Get-Command mt.exe -ErrorAction SilentlyContinue
  if ($tool) {
    $manifestTool = $tool.Source
  } else {
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $tools = @(Get-ChildItem (Join-Path $kits "*\x64\mt.exe") -ErrorAction SilentlyContinue |
      Sort-Object FullName -Descending)
    if ($tools.Count -eq 0) { throw "mt.exe was not found in PATH or the Windows SDK." }
    $manifestTool = $tools[0].FullName
  }

  & $manifestTool -nologo -manifest $manifest "-outputresource:$executable;#1"
  if ($LASTEXITCODE -ne 0) { throw "Embedding the library test manifest failed ($LASTEXITCODE)." }
  $checkPath = [System.IO.Path]::GetTempFileName()
  try {
    & $manifestTool -nologo "-inputresource:$executable;#1" "-out:$checkPath"
    if ($LASTEXITCODE -ne 0) { throw "Reading the library test manifest failed ($LASTEXITCODE)." }
    [xml]$embedded = Get-Content -LiteralPath $checkPath -Raw
    $controls = @($embedded.SelectNodes("//*[local-name()='assemblyIdentity' and @name='Microsoft.Windows.Common-Controls']"))
    if ($controls.Count -ne 1 -or $controls[0].version -ne "6.0.0.0" -or
        $embedded.SelectNodes("//*[local-name()='requestedExecutionLevel']").Count -ne 0) {
      throw "The library test manifest differs from the main app's Common Controls context."
    }
  } finally {
    Remove-Item -LiteralPath $checkPath -Force
  }
  Write-Host "Library test manifest verified: Common Controls 6, no UAC declaration."
}

# Windows PowerShell may return immediately for GUI-subsystem test binaries
# (including the update-helper harness), leaving LASTEXITCODE unset. Wait on
# the actual process and drain both redirected streams for every target.
function Quote-NativeArgument([string]$Argument) {
  '"' + ($Argument -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
$startInfo = New-Object System.Diagnostics.ProcessStartInfo
$startInfo.FileName = $executable
$startInfo.UseShellExecute = $false
$startInfo.Arguments = (@($TestArguments | ForEach-Object { Quote-NativeArgument $_ }) -join ' ')
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$process = New-Object System.Diagnostics.Process
$process.StartInfo = $startInfo
try {
  if (-not $process.Start()) { throw "The test executable did not start." }
  $stdout = $process.StandardOutput.ReadToEndAsync()
  $stderr = $process.StandardError.ReadToEndAsync()
  $process.WaitForExit()
  [Console]::Out.Write($stdout.GetAwaiter().GetResult())
  [Console]::Error.Write($stderr.GetAwaiter().GetResult())
  $testExit = $process.ExitCode
} finally {
  $process.Dispose()
}
exit $testExit
