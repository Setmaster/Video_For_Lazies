param(
  [Parameter(Mandatory = $true)][string]$PortableDir,
  [string]$SmokeRoot,
  [string]$ScreenshotPath,
  [int]$TimeoutSeconds = 60,
  [double]$TrimEndSeconds = 1.25,
  [int]$InputWidth = 640,
  [int]$InputHeight = 360,
  [double]$InputRate = 30,
  [int]$InputVideoBitrateKbps = 900,
  [ValidateSet("mp4", "webm", "mp3")][string]$OutputFormat = "mp4",
  [double]$SizeLimitMb = 0
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function New-SmokeRoot {
  $tmp = [System.IO.Path]::GetTempPath()
  $stamp = Get-Date -Format "yyyy-MM-dd_HH-mm-ss"
  Join-Path $tmp "vfl-portable-export-smoke-$stamp"
}

function New-SmokeScreenshotPath {
  param([string]$Root)

  Join-Path $Root "portable-export-smoke.png"
}

Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class VflPortableExportSmokeNative {
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT {
    public int Left;
    public int Top;
    public int Right;
    public int Bottom;
  }

  [DllImport("user32.dll")]
  public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);

  [DllImport("user32.dll")]
  public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);

  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);

  [DllImport("user32.dll")]
  public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, int nFlags);
}
"@

function Capture-WindowToPng {
  param(
    [IntPtr]$Handle,
    [string]$DestinationPath
  )

  if (-not $Handle -or $Handle -eq [IntPtr]::Zero) {
    return
  }

  $rect = New-Object VflPortableExportSmokeNative+RECT
  if (-not [VflPortableExportSmokeNative]::GetWindowRect($Handle, [ref]$rect)) {
    return
  }

  $width = $rect.Right - $rect.Left
  $height = $rect.Bottom - $rect.Top
  if ($width -le 0 -or $height -le 0) {
    return
  }

  $parent = Split-Path -Parent $DestinationPath
  if ($parent) {
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
  }

  $bitmap = New-Object System.Drawing.Bitmap($width, $height)
  $graphics = [System.Drawing.Graphics]::FromImage($bitmap)

  try {
    $hdc = $graphics.GetHdc()
    try {
      $printed = [VflPortableExportSmokeNative]::PrintWindow($Handle, $hdc, 2)
      if (-not $printed) {
        $printed = [VflPortableExportSmokeNative]::PrintWindow($Handle, $hdc, 0)
      }
    } finally {
      $graphics.ReleaseHdc($hdc)
    }

    if (-not $printed) {
      $source = New-Object System.Drawing.Point($rect.Left, $rect.Top)
      $target = [System.Drawing.Point]::Empty
      $size = New-Object System.Drawing.Size($width, $height)
      $graphics.CopyFromScreen($source, $target, $size)
    }

    $bitmap.Save($DestinationPath, [System.Drawing.Imaging.ImageFormat]::Png)
  } finally {
    $graphics.Dispose()
    $bitmap.Dispose()
  }
}

function Get-JsonFileOrNull {
  param([string]$Path)

  if (-not (Test-Path $Path)) {
    return $null
  }

  try {
    return Get-Content -Path $Path -Raw | ConvertFrom-Json
  } catch {
    return $null
  }
}

$portableRoot = [System.IO.Path]::GetFullPath($PortableDir)
$exePath = Join-Path $portableRoot "Video_For_Lazies.exe"
$ffmpegPath = Join-Path $portableRoot "ffmpeg-sidecar\\ffmpeg.exe"
$ffprobePath = Join-Path $portableRoot "ffmpeg-sidecar\\ffprobe.exe"

foreach ($requiredPath in @($exePath, $ffmpegPath, $ffprobePath)) {
  if (-not (Test-Path $requiredPath)) {
    throw "Portable export smoke could not find required file: $requiredPath"
  }
}

if (-not $SmokeRoot) {
  $SmokeRoot = New-SmokeRoot
}
$SmokeRoot = [System.IO.Path]::GetFullPath($SmokeRoot)
New-Item -ItemType Directory -Path $SmokeRoot -Force | Out-Null

if (-not $ScreenshotPath) {
  $ScreenshotPath = New-SmokeScreenshotPath -Root $SmokeRoot
}
$ScreenshotPath = [System.IO.Path]::GetFullPath($ScreenshotPath)

$inputPath = Join-Path $SmokeRoot "smoke-input.webm"
$outputPath = Join-Path $SmokeRoot ("smoke-output.{0}" -f $OutputFormat)
$statusPath = Join-Path $SmokeRoot "smoke-status.json"

Remove-Item $inputPath, $outputPath, $statusPath -Force -ErrorAction SilentlyContinue

& $ffmpegPath `
  -y `
  -hide_banner `
  -loglevel error `
  -f lavfi `
  -i ("testsrc2=size={0}x{1}:rate={2}" -f $InputWidth, $InputHeight, $InputRate.ToString([System.Globalization.CultureInfo]::InvariantCulture)) `
  -f lavfi `
  -i "sine=frequency=880:sample_rate=48000" `
  -t 2 `
  -c:v libvpx `
  -deadline good `
  -cpu-used 8 `
  -b:v ("{0}k" -f $InputVideoBitrateKbps) `
  -c:a libvorbis `
  -q:a 4 `
  $inputPath

if ($LASTEXITCODE -ne 0 -or -not (Test-Path $inputPath)) {
  throw "Portable export smoke failed to generate the synthetic input clip."
}

$startInfo = New-Object System.Diagnostics.ProcessStartInfo
$startInfo.FileName = $exePath
$startInfo.WorkingDirectory = $portableRoot
$startInfo.UseShellExecute = $false
$startInfo.EnvironmentVariables["VFL_SMOKE_INPUT"] = $inputPath
$startInfo.EnvironmentVariables["VFL_SMOKE_OUTPUT"] = $outputPath
$startInfo.EnvironmentVariables["VFL_SMOKE_STATUS"] = $statusPath
$startInfo.EnvironmentVariables["VFL_SMOKE_FORMAT"] = $OutputFormat
$startInfo.EnvironmentVariables["VFL_SMOKE_SIZE_LIMIT_MB"] = $SizeLimitMb.ToString([System.Globalization.CultureInfo]::InvariantCulture)
$startInfo.EnvironmentVariables["VFL_SMOKE_TRIM_START_S"] = "0"
$startInfo.EnvironmentVariables["VFL_SMOKE_TRIM_END_S"] = $TrimEndSeconds.ToString([System.Globalization.CultureInfo]::InvariantCulture)

$process = [System.Diagnostics.Process]::Start($startInfo)
if (-not $process) {
  throw "Portable export smoke could not launch the packaged app."
}

$lastStatus = $null
$lastStageName = $null
$requiredStages = @("input-applied", "probe-ready", "preview-ready", "interaction-ready", "encoding", "success")

try {
  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  do {
    Start-Sleep -Milliseconds 500
    $process.Refresh()
    $status = Get-JsonFileOrNull -Path $statusPath
    if ($status) {
      $lastStatus = $status
      if ($status.stage -ne $lastStageName) {
        Write-Host ("Portable export smoke stage: {0}" -f $status.stage)
        $lastStageName = $status.stage
      }
      if ($status.stage -eq "success") {
        break
      }
      if ($status.stage -eq "error") {
        $statusMessage = if ($status.message) { $status.message } else { "Unknown smoke failure." }
        throw ("Portable export smoke failed inside the app: {0}" -f $statusMessage)
      }
    }

    if ($process.HasExited -and (-not $status -or $status.stage -ne "success")) {
      throw "Portable export smoke app exited before reporting success."
    }
  } while ((Get-Date) -lt $deadline)

  if (-not $lastStatus -or $lastStatus.stage -ne "success") {
    $lastStage = if ($lastStatus -and $lastStatus.stage) { $lastStatus.stage } else { "none" }
    throw ("Portable export smoke timed out waiting for success. Last stage: {0}" -f $lastStage)
  }

  if (-not (Test-Path $outputPath)) {
    throw "Portable export smoke reported success but no output file was written."
  }

  $stageHistory = @()
  if ($lastStatus.PSObject.Properties.Name -contains "stageHistory" -and $lastStatus.stageHistory) {
    $stageHistory = @($lastStatus.stageHistory)
  }

  if ($stageHistory.Count -eq 0) {
    throw "Portable export smoke reported success without any persisted stageHistory."
  }

  $missingStages = @()
  foreach ($requiredStage in $requiredStages) {
    if ($stageHistory -notcontains $requiredStage) {
      $missingStages += $requiredStage
    }
  }
  if ($missingStages.Count -gt 0) {
    throw ("Portable export smoke missed required app stages: {0}. Saw: {1}" -f ($missingStages -join ", "), ($stageHistory -join " -> "))
  }

  $outputItem = Get-Item $outputPath
  if ($outputItem.Length -le 0) {
    throw "Portable export smoke wrote an empty output file."
  }

  $ffprobeJson = & $ffprobePath `
    -v quiet `
    -print_format json `
    -show_format `
    -show_streams `
    $outputPath

  if ($LASTEXITCODE -ne 0) {
    throw "Portable export smoke wrote an output file that ffprobe could not read."
  }

  $ffprobe = $ffprobeJson | ConvertFrom-Json
  $outputDurationS = [double]::Parse($ffprobe.format.duration, [System.Globalization.CultureInfo]::InvariantCulture)
  $trimStartS = $null
  $trimEndS = $null
  $expectedDurationS = $null

  if ($null -ne $lastStatus.expectedDurationS -and $null -ne $lastStatus.trimStartS -and $null -ne $lastStatus.trimEndS) {
    $expectedDurationS = [double]$lastStatus.expectedDurationS
    $trimStartS = [double]$lastStatus.trimStartS
    $trimEndS = [double]$lastStatus.trimEndS
  } elseif ($lastStatus.message -match 'start at ([0-9]+(?:\.[0-9]+)?)s, end at ([0-9]+(?:\.[0-9]+)?)s') {
    $trimStartS = [double]::Parse($matches[1], [System.Globalization.CultureInfo]::InvariantCulture)
    $trimEndS = [double]::Parse($matches[2], [System.Globalization.CultureInfo]::InvariantCulture)
    $expectedDurationS = $trimEndS - $trimStartS
  } else {
    throw "Portable export smoke reported success without persisted trim metrics or a parsable interaction summary."
  }

  if ($trimEndS -le $trimStartS) {
    throw ("Portable export smoke reported invalid trim metrics: start={0} end={1}" -f $trimStartS, $trimEndS)
  }

  if ([Math]::Abs($outputDurationS - $expectedDurationS) -gt 0.18) {
    throw ("Portable export smoke output duration mismatch. expected={0:N3}s actual={1:N3}s" -f $expectedDurationS, $outputDurationS)
  }

  Write-Host ("Portable export smoke passed. output={0} size_bytes={1} duration_s={2:N3} trim={3:N3}-{4:N3} status={5} stages={6}" -f $outputPath, $outputItem.Length, $outputDurationS, $trimStartS, $trimEndS, $statusPath, ($stageHistory -join " -> "))
} catch {
  try {
    $process.Refresh()
    if (-not $process.HasExited) {
      [VflPortableExportSmokeNative]::ShowWindowAsync($process.MainWindowHandle, 5) | Out-Null
      [VflPortableExportSmokeNative]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
      Start-Sleep -Milliseconds 300
      Capture-WindowToPng -Handle $process.MainWindowHandle -DestinationPath $ScreenshotPath
      if (Test-Path $ScreenshotPath) {
        Write-Host "Portable export smoke captured failure screenshot: $ScreenshotPath"
      }
    }
  } catch {
    Write-Host "Portable export smoke could not capture a failure screenshot."
  }
  throw
} finally {
  if ($process -and -not $process.HasExited) {
    Stop-Process -Id $process.Id -Force
  }
}
