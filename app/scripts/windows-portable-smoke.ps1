param(
  [Parameter(Mandatory = $true)][string]$PortableDir,
  [string]$ScreenshotPath,
  [int]$StartupTimeoutSeconds = 20,
  [int]$InitialDelaySeconds = 5,
  [int]$RetryDelaySeconds = 3,
  [int]$MaxAttempts = 3,
  [double]$MaxAverageLuminance = 120,
  [double]$MaxBrightRatio = 0.10,
  [double]$MinDarkRatio = 0.35
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function New-SmokeScreenshotPath {
  $tmp = [System.IO.Path]::GetTempPath()
  $stamp = Get-Date -Format "yyyy-MM-dd_HH-mm-ss"
  Join-Path $tmp "vfl-portable-smoke-$stamp.png"
}

function New-AttemptScreenshotPath {
  param(
    [string]$BasePath,
    [int]$Attempt
  )

  $dir = Split-Path -Parent $BasePath
  $name = [System.IO.Path]::GetFileNameWithoutExtension($BasePath)
  $ext = [System.IO.Path]::GetExtension($BasePath)
  Join-Path $dir "$name-attempt$Attempt$ext"
}

Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class VflPortableSmokeNative {
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

  $rect = New-Object VflPortableSmokeNative+RECT
  if (-not [VflPortableSmokeNative]::GetWindowRect($Handle, [ref]$rect)) {
    throw "Failed to read window bounds for smoke screenshot."
  }

  $width = $rect.Right - $rect.Left
  $height = $rect.Bottom - $rect.Top
  if ($width -le 0 -or $height -le 0) {
    throw "Portable smoke saw invalid window bounds ${width}x${height}."
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
      $printed = [VflPortableSmokeNative]::PrintWindow($Handle, $hdc, 2)
      if (-not $printed) {
        $printed = [VflPortableSmokeNative]::PrintWindow($Handle, $hdc, 0)
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

function Get-ImageLuminanceStats {
  param([string]$ImagePath)

  $bitmap = [System.Drawing.Bitmap]::FromFile($ImagePath)
  try {
    $startY = [Math]::Min(40, [Math]::Max($bitmap.Height - 1, 0))
    $sampleHeight = [Math]::Max($bitmap.Height - $startY, 1)
    $stepX = [Math]::Max([int]($bitmap.Width / 120), 1)
    $stepY = [Math]::Max([int]($sampleHeight / 120), 1)

    $sampleCount = 0
    $darkCount = 0
    $brightCount = 0
    $luminanceSum = 0.0

    for ($y = $startY; $y -lt $bitmap.Height; $y += $stepY) {
      for ($x = 0; $x -lt $bitmap.Width; $x += $stepX) {
        $pixel = $bitmap.GetPixel($x, $y)
        $luminance = (0.2126 * $pixel.R) + (0.7152 * $pixel.G) + (0.0722 * $pixel.B)
        $luminanceSum += $luminance
        if ($luminance -lt 80) {
          $darkCount++
        }
        if ($luminance -gt 220) {
          $brightCount++
        }
        $sampleCount++
      }
    }

    [pscustomobject]@{
      Width = $bitmap.Width
      Height = $bitmap.Height
      AvgLuminance = [Math]::Round($luminanceSum / [Math]::Max($sampleCount, 1), 2)
      DarkRatio = [Math]::Round($darkCount / [Math]::Max($sampleCount, 1), 3)
      BrightRatio = [Math]::Round($brightCount / [Math]::Max($sampleCount, 1), 3)
    }
  } finally {
    $bitmap.Dispose()
  }
}

function Test-HealthyStartupSurface {
  param($Stats)

  return (
    $Stats.AvgLuminance -le $MaxAverageLuminance -and
    $Stats.BrightRatio -le $MaxBrightRatio -and
    $Stats.DarkRatio -ge $MinDarkRatio
  )
}

$portableRoot = [System.IO.Path]::GetFullPath($PortableDir)
$exePath = Join-Path $portableRoot "Video_For_Lazies.exe"
if (-not (Test-Path $exePath)) {
  throw "Portable smoke could not find app executable at $exePath"
}

if (-not $ScreenshotPath) {
  $ScreenshotPath = New-SmokeScreenshotPath
}
$ScreenshotPath = [System.IO.Path]::GetFullPath($ScreenshotPath)

$process = $null
$finalShot = $null
$lastStats = $null
$passed = $false

try {
  $process = Start-Process -FilePath $exePath -WorkingDirectory $portableRoot -PassThru
  $deadline = (Get-Date).AddSeconds($StartupTimeoutSeconds)

  do {
    Start-Sleep -Milliseconds 500
    $process.Refresh()
    if ($process.HasExited) {
      throw "Portable smoke app exited before opening a window."
    }
  } while ($process.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline)

  if ($process.MainWindowHandle -eq 0) {
    throw "Portable smoke timed out waiting for the app window."
  }

  $shell = New-Object -ComObject WScript.Shell

  for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
    if ($process.HasExited) {
      throw "Portable smoke app exited during startup verification."
    }

    $delay = if ($attempt -eq 1) { $InitialDelaySeconds } else { $RetryDelaySeconds }
    Start-Sleep -Seconds $delay

    $null = $shell.AppActivate($process.Id)
    [VflPortableSmokeNative]::ShowWindowAsync($process.MainWindowHandle, 5) | Out-Null
    [VflPortableSmokeNative]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 500

    $attemptShot = if ($MaxAttempts -gt 1) {
      New-AttemptScreenshotPath -BasePath $ScreenshotPath -Attempt $attempt
    } else {
      $ScreenshotPath
    }

    Capture-WindowToPng -Handle $process.MainWindowHandle -DestinationPath $attemptShot
    $stats = Get-ImageLuminanceStats -ImagePath $attemptShot
    $finalShot = $attemptShot
    $lastStats = $stats

    Write-Host ("Portable smoke attempt {0}: avg_luminance={1}, dark_ratio={2}, bright_ratio={3}, screenshot={4}" -f `
      $attempt, $stats.AvgLuminance, $stats.DarkRatio, $stats.BrightRatio, $attemptShot)

    if (Test-HealthyStartupSurface -Stats $stats) {
      Write-Host "Portable startup smoke passed."
      $passed = $true
      break
    }
  }

  if (-not $passed) {
    throw ("Portable startup smoke failed: avg_luminance={0}, dark_ratio={1}, bright_ratio={2}, screenshot={3}" -f `
      $lastStats.AvgLuminance, $lastStats.DarkRatio, $lastStats.BrightRatio, $finalShot)
  }
} finally {
  if ($process -and -not $process.HasExited) {
    Stop-Process -Id $process.Id -Force
  }
}
