param(
  [Parameter(Mandatory = $true)]
  [string]$HelperPath,

  [string]$MainAppPath,

  [switch]$RunSelfTest
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-ManifestTool {
  $command = Get-Command mt.exe -ErrorAction SilentlyContinue
  if ($command) {
    return $command.Source
  }

  $kitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
  $candidates = @(
    Get-ChildItem (Join-Path $kitsRoot "*\x64\mt.exe") -ErrorAction SilentlyContinue |
      Sort-Object FullName -Descending
  )
  if ($candidates.Count -eq 0) {
    throw "mt.exe was not found in PATH or the Windows 10 SDK."
  }
  return $candidates[0].FullName
}

function Export-EmbeddedManifest {
  param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestTool,

    [Parameter(Mandatory = $true)]
    [string]$ExecutablePath,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
  )

  $resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).Path
  & $ManifestTool -nologo "-inputresource:$resolvedExecutable;#1" "-out:$OutputPath"
  if ($LASTEXITCODE -ne 0) {
    throw "mt.exe failed to extract the manifest from $resolvedExecutable (exit $LASTEXITCODE)."
  }

  [xml](Get-Content -LiteralPath $OutputPath -Raw)
}

function Get-RequestedExecutionLevels {
  param(
    [Parameter(Mandatory = $true)]
    [xml]$Manifest
  )

  @($Manifest.SelectNodes("//*[local-name()='requestedExecutionLevel']"))
}

$manifestTool = Resolve-ManifestTool
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "vfl-helper-manifest-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $tempRoot | Out-Null
$verificationSucceeded = $false

try {
  $helperManifest = Export-EmbeddedManifest `
    -ManifestTool $manifestTool `
    -ExecutablePath $HelperPath `
    -OutputPath (Join-Path $tempRoot "helper.manifest")
  $helperLevels = @(Get-RequestedExecutionLevels -Manifest $helperManifest)
  if ($helperLevels.Count -ne 1) {
    throw "The update helper must contain exactly one requestedExecutionLevel; found $($helperLevels.Count)."
  }
  if ($helperLevels[0].level -ne "asInvoker" -or $helperLevels[0].uiAccess -ne "false") {
    throw "The update helper must request level=asInvoker and uiAccess=false."
  }

  $helperIdentity = @(
    $helperManifest.SelectNodes("//*[local-name()='assemblyIdentity' and @name='com.setmaster.VideoForLazies.UpdateHelper']")
  )
  if ($helperIdentity.Count -ne 1) {
    throw "The update helper manifest identity is missing or duplicated."
  }

  if ($MainAppPath) {
    $mainManifest = Export-EmbeddedManifest `
      -ManifestTool $manifestTool `
      -ExecutablePath $MainAppPath `
      -OutputPath (Join-Path $tempRoot "main.manifest")
    $mainLevels = @(Get-RequestedExecutionLevels -Manifest $mainManifest)
    if ($mainLevels.Count -ne 0) {
      throw "The main app manifest unexpectedly contains a requestedExecutionLevel intended only for the helper."
    }
    if ($mainManifest.OuterXml.Contains("com.setmaster.VideoForLazies.UpdateHelper")) {
      throw "The helper manifest identity leaked into the main app executable."
    }
    $commonControls = @(
      $mainManifest.SelectNodes("//*[local-name()='assemblyIdentity' and @name='Microsoft.Windows.Common-Controls']")
    )
    if ($commonControls.Count -ne 1) {
      throw "The main app manifest no longer contains exactly one Common-Controls dependency."
    }
  }

  if ($RunSelfTest) {
    $selfTestOutput = @(& (Resolve-Path -LiteralPath $HelperPath).Path --self-test 2>&1)
    if ($LASTEXITCODE -ne 0) {
      throw "The update helper self-test failed with exit code $LASTEXITCODE."
    }
    if (($selfTestOutput -join "`n").Trim() -ne "vfl-update-helper ok") {
      throw "The update helper self-test returned an unexpected response."
    }
  }

  $verificationSucceeded = $true
  Write-Output "Windows update helper manifest verified: one asInvoker declaration, uiAccess=false, helper-only identity."
} finally {
  if ($verificationSucceeded) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
  } else {
    Write-Warning "Preserving updater manifest diagnostics at $tempRoot"
  }
}
