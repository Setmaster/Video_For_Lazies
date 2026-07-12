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
  [double]$SizeLimitMb = 0,
  [ValidateSet("source", "maxEdge", "custom")][string]$ResizeMode,
  [int]$ResizeMaxEdgePx,
  [int]$ResizeWidthPx,
  [int]$ResizeHeightPx
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
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
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
  public static extern IntPtr GetForegroundWindow();

  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

  [DllImport("kernel32.dll")]
  public static extern uint GetCurrentThreadId();

  [DllImport("user32.dll")]
  public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool attach);

  [DllImport("user32.dll")]
  public static extern bool BringWindowToTop(IntPtr hWnd);

  [StructLayout(LayoutKind.Sequential)]
  public struct MOUSEINPUT {
    public int dx;
    public int dy;
    public uint mouseData;
    public uint dwFlags;
    public uint time;
    public UIntPtr dwExtraInfo;
  }

  [StructLayout(LayoutKind.Sequential)]
  public struct KEYBDINPUT {
    public ushort wVk;
    public ushort wScan;
    public uint dwFlags;
    public uint time;
    public UIntPtr dwExtraInfo;
  }

  [StructLayout(LayoutKind.Sequential)]
  public struct HARDWAREINPUT {
    public uint uMsg;
    public ushort wParamL;
    public ushort wParamH;
  }

  [StructLayout(LayoutKind.Explicit)]
  public struct INPUTUNION {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
    [FieldOffset(0)] public HARDWAREINPUT hi;
  }

  [StructLayout(LayoutKind.Sequential)]
  public struct INPUT {
    public uint type;
    public INPUTUNION data;
  }

  [DllImport("user32.dll", SetLastError = true)]
  private static extern uint SendInput(uint inputCount, [In] INPUT[] inputs, int inputSize);

  public static void SendVirtualKey(IntPtr expectedHandle, ushort virtualKey, bool extended) {
    if (GetForegroundWindow() != expectedHandle) {
      throw new InvalidOperationException("Portable export smoke lost foreground immediately before native keyboard input.");
    }

    const uint INPUT_KEYBOARD = 1;
    const uint KEYEVENTF_EXTENDEDKEY = 0x0001;
    const uint KEYEVENTF_KEYUP = 0x0002;
    uint flags = extended ? KEYEVENTF_EXTENDEDKEY : 0;
    INPUT[] inputs = new INPUT[1];
    inputs[0].type = INPUT_KEYBOARD;
    inputs[0].data.ki.wVk = virtualKey;
    inputs[0].data.ki.dwFlags = flags;

    uint downSent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT)));
    if (downSent != (uint)inputs.Length) {
      int error = Marshal.GetLastWin32Error();
      throw new InvalidOperationException(
        String.Format("Portable export smoke native keyboard input inserted {0}/1 key-down events (Win32 error {1}).", downSent, error)
      );
    }

    System.Threading.Thread.Sleep(50);
    inputs[0].data.ki.dwFlags = flags | KEYEVENTF_KEYUP;
    uint upSent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT)));
    if (upSent != (uint)inputs.Length) {
      int error = Marshal.GetLastWin32Error();
      throw new InvalidOperationException(
        String.Format("Portable export smoke native keyboard input inserted {0}/1 key-up events (Win32 error {1}).", upSent, error)
      );
    }
  }

  public static bool ForceForegroundWindow(IntPtr hWnd) {
    IntPtr foreground = GetForegroundWindow();
    uint ignored;
    uint foregroundThread = GetWindowThreadProcessId(foreground, out ignored);
    uint targetThread = GetWindowThreadProcessId(hWnd, out ignored);
    uint currentThread = GetCurrentThreadId();
    bool attachedForeground = foregroundThread != 0 && foregroundThread != currentThread &&
      AttachThreadInput(currentThread, foregroundThread, true);
    bool attachedTarget = targetThread != 0 && targetThread != currentThread && targetThread != foregroundThread &&
      AttachThreadInput(currentThread, targetThread, true);
    try {
      ShowWindowAsync(hWnd, 9);
      BringWindowToTop(hWnd);
      SetForegroundWindow(hWnd);
      return GetForegroundWindow() == hWnd;
    } finally {
      if (attachedTarget) AttachThreadInput(currentThread, targetThread, false);
      if (attachedForeground) AttachThreadInput(currentThread, foregroundThread, false);
    }
  }

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

function Invoke-SmokeAutomationElement {
  param(
    [IntPtr]$Handle,
    [string]$Name
  )

  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Handle)
  $nameCondition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::NameProperty,
    $Name
  )
  $invokableCondition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::IsInvokePatternAvailableProperty,
    $true
  )
  $condition = New-Object System.Windows.Automation.AndCondition($nameCondition, $invokableCondition)
  for ($attempt = 0; $attempt -lt 20; $attempt++) {
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($element) {
      try {
        $pattern = $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
        if ($pattern) {
          ([System.Windows.Automation.InvokePattern]$pattern).Invoke()
          return
        }
      } catch {
        # Retry while WebView accessibility state catches up with the mounted control.
      }
    }
    Start-Sleep -Milliseconds 100
  }
  throw ("Portable export smoke could not invoke accessible control: {0}" -f $Name)
}

function Test-SmokeAutomationElementFocus {
  param(
    [IntPtr]$Handle,
    [System.Windows.Automation.AutomationElement]$Element
  )

  try {
    if ([VflPortableExportSmokeNative]::GetForegroundWindow() -ne $Handle) {
      return $false
    }
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
    if ($null -eq $focused) { return $false }
    $expectedAutomationId = $Element.Current.AutomationId
    $expectedName = $Element.Current.Name
    $identityMatches = if ($expectedAutomationId) {
      $focused.Current.AutomationId -eq $expectedAutomationId
    } else {
      $focused.Current.Name -eq $expectedName
    }
    return $focused.Current.HasKeyboardFocus -and
      $focused.Current.ProcessId -eq $Element.Current.ProcessId -and
      $identityMatches
  } catch {
    return $false
  }
}

function Wait-SmokeStableAutomationElementFocus {
  param(
    [IntPtr]$Handle,
    [System.Windows.Automation.AutomationElement]$Element
  )

  $stableSamples = 0
  for ($attempt = 0; $attempt -lt 12; $attempt++) {
    if (Test-SmokeAutomationElementFocus -Handle $Handle -Element $Element) {
      $stableSamples++
      if ($stableSamples -eq 3) {
        return $true
      }
    } else {
      $stableSamples = 0
    }
    Start-Sleep -Milliseconds 100
  }
  return $false
}

function Set-SmokeAutomationElementFocus {
  param(
    [IntPtr]$Handle,
    [string]$Name,
    [string]$AutomationId
  )

  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Handle)
  $property = if ($AutomationId) {
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty
  } else {
    [System.Windows.Automation.AutomationElement]::NameProperty
  }
  $expected = if ($AutomationId) { $AutomationId } else { $Name }
  $identityCondition = New-Object System.Windows.Automation.PropertyCondition($property, $expected)
  $focusableCondition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::IsKeyboardFocusableProperty,
    $true
  )
  $condition = New-Object System.Windows.Automation.AndCondition($identityCondition, $focusableCondition)
  $element = $null
  for ($attempt = 0; $attempt -lt 20 -and -not $element; $attempt++) {
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if (-not $element) { Start-Sleep -Milliseconds 100 }
  }
  if (-not $element) {
    throw ("Portable export smoke could not find accessible keyboard target: {0}" -f $expected)
  }

  if (Wait-SmokeStableAutomationElementFocus -Handle $Handle -Element $element) {
    return $element
  }

  try {
    $element.SetFocus()
  } catch {
    throw ("Portable export smoke could not request focus for accessible keyboard target: {0}" -f $expected)
  }
  if (Wait-SmokeStableAutomationElementFocus -Handle $Handle -Element $element) {
    return $element
  }
  throw ("Portable export smoke could not establish stable WebView keyboard focus: {0}" -f $expected)
}

function Set-SmokeProcessForeground {
  param([System.Diagnostics.Process]$Process)

  $handle = [IntPtr]::Zero
  for ($attempt = 0; $attempt -lt 20; $attempt++) {
    $Process.Refresh()
    $handle = $Process.MainWindowHandle
    if ($handle -and $handle -ne [IntPtr]::Zero) {
      break
    }
    Start-Sleep -Milliseconds 100
  }
  if (-not $handle -or $handle -eq [IntPtr]::Zero) {
    throw "Portable export smoke could not find the app window for real keyboard input."
  }

  [void][VflPortableExportSmokeNative]::ShowWindowAsync($handle, 9)
  $foreground = $false
  for ($attempt = 0; $attempt -lt 10; $attempt++) {
    [void][VflPortableExportSmokeNative]::SetForegroundWindow($handle)
    Start-Sleep -Milliseconds 100
    if ([VflPortableExportSmokeNative]::GetForegroundWindow() -eq $handle) {
      $foreground = $true
      break
    }
  }
  if (-not $foreground) {
    $shell = New-Object -ComObject WScript.Shell
    [void]$shell.AppActivate($Process.Id)
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
      Start-Sleep -Milliseconds 100
      if ([VflPortableExportSmokeNative]::GetForegroundWindow() -eq $handle) {
        $foreground = $true
        break
      }
    }
  }
  if (-not $foreground) {
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
      if ([VflPortableExportSmokeNative]::ForceForegroundWindow($handle)) {
        $foreground = $true
        break
      }
      Start-Sleep -Milliseconds 100
    }
  }
  if (-not $foreground) {
    throw "Portable export smoke could not foreground the app for real keyboard input."
  }
  Start-Sleep -Milliseconds 50
  return $handle
}

function Send-SmokeKeySequence {
  param(
    [System.Diagnostics.Process]$Process,
    [string[]]$Keys,
    [string]$AutomationName,
    [string]$AutomationId
  )

  $handle = Set-SmokeProcessForeground -Process $Process
  foreach ($key in $Keys) {
    $targetElement = $null
    if ([VflPortableExportSmokeNative]::GetForegroundWindow() -ne $handle) {
      [void][VflPortableExportSmokeNative]::ForceForegroundWindow($handle)
      Start-Sleep -Milliseconds 100
      if ([VflPortableExportSmokeNative]::GetForegroundWindow() -ne $handle) {
        throw "Portable export smoke could not restore foreground before real keyboard input."
      }
    }
    if ($AutomationName -or $AutomationId) {
      $targetElement = Set-SmokeAutomationElementFocus -Handle $handle -Name $AutomationName -AutomationId $AutomationId
      if (-not (Test-SmokeAutomationElementFocus -Handle $handle -Element $targetElement)) {
        throw "Portable export smoke lost stable WebView keyboard focus immediately before native input."
      }
    }
    $virtualKeys = @{
      "{TAB}" = [UInt16]0x09
      "{LEFT}" = [UInt16]0x25
      "{UP}" = [UInt16]0x26
      "{RIGHT}" = [UInt16]0x27
    }
    if (-not $virtualKeys.ContainsKey($key)) {
      throw ("Portable export smoke does not support keyboard token: {0}" -f $key)
    }
    $isExtendedKey = $key -in @("{LEFT}", "{UP}", "{RIGHT}")
    [VflPortableExportSmokeNative]::SendVirtualKey($handle, [UInt16]$virtualKeys[$key], $isExtendedKey)
    Start-Sleep -Milliseconds 180
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
$webViewDataRoot = Join-Path $SmokeRoot "webview2-user-data"

Remove-Item $inputPath, $outputPath, $statusPath -Force -ErrorAction SilentlyContinue
Remove-Item $webViewDataRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $webViewDataRoot -Force | Out-Null

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
$startInfo.EnvironmentVariables["VFL_SMOKE_TRIM_START_S"] = "0.25"
$startInfo.EnvironmentVariables["VFL_SMOKE_TRIM_END_S"] = $TrimEndSeconds.ToString([System.Globalization.CultureInfo]::InvariantCulture)
$startInfo.EnvironmentVariables["VFL_SMOKE_WORKFLOW_QUEUE"] = "1"
$startInfo.EnvironmentVariables["WEBVIEW2_USER_DATA_FOLDER"] = $webViewDataRoot
if ($ResizeMode) {
  $startInfo.EnvironmentVariables["VFL_SMOKE_RESIZE_MODE"] = $ResizeMode
}
if ($ResizeMaxEdgePx -gt 0) {
  $startInfo.EnvironmentVariables["VFL_SMOKE_RESIZE_MAX_EDGE_PX"] = $ResizeMaxEdgePx.ToString([System.Globalization.CultureInfo]::InvariantCulture)
}
if ($ResizeWidthPx -gt 0) {
  $startInfo.EnvironmentVariables["VFL_SMOKE_RESIZE_WIDTH_PX"] = $ResizeWidthPx.ToString([System.Globalization.CultureInfo]::InvariantCulture)
}
if ($ResizeHeightPx -gt 0) {
  $startInfo.EnvironmentVariables["VFL_SMOKE_RESIZE_HEIGHT_PX"] = $ResizeHeightPx.ToString([System.Globalization.CultureInfo]::InvariantCulture)
}

$process = [System.Diagnostics.Process]::Start($startInfo)
if (-not $process) {
  throw "Portable export smoke could not launch the packaged app."
}

$lastStatus = $null
$lastStageName = $null
$sentKeyboardStages = @{}
$requiredStages = @(
  "input-applied",
  "probe-ready",
  "workflow-recipe-ready",
  "workflow-recipe-saved",
  "workflow-queue-ready",
  "workflow-queue-complete",
  "workflow-ready",
  "preview-ready",
  "keyboard-trim-ready",
  "keyboard-trim-incremented",
  "keyboard-trim-complete",
  "keyboard-crop-ready",
  "keyboard-crop-complete",
  "keyboard-modal-ready",
  "keyboard-modal-open",
  "keyboard-complete",
  "accessibility-ready",
  "interaction-ready",
  "encoding",
  "success"
)

try {
  $null = Set-SmokeProcessForeground -Process $process
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
      if (-not $sentKeyboardStages.ContainsKey($status.stage)) {
        switch ($status.stage) {
          "workflow-recipe-ready" {
            Invoke-SmokeAutomationElement -Handle $process.MainWindowHandle -Name "Create recipe from current settings"
            $sentKeyboardStages[$status.stage] = $true
          }
          "workflow-queue-ready" {
            Invoke-SmokeAutomationElement -Handle $process.MainWindowHandle -Name "Run queue"
            $sentKeyboardStages[$status.stage] = $true
          }
          "keyboard-trim-ready" {
            Send-SmokeKeySequence -Process $process -Keys @("{RIGHT}") -AutomationId "vfl-trim-start-slider"
            $sentKeyboardStages[$status.stage] = $true
          }
          "keyboard-trim-incremented" {
            Send-SmokeKeySequence -Process $process -Keys @("{LEFT}") -AutomationId "vfl-trim-start-slider"
            Send-SmokeKeySequence -Process $process -Keys @("{TAB}") -AutomationId "vfl-trim-start-slider"
            $sentKeyboardStages[$status.stage] = $true
          }
          "keyboard-crop-ready" {
            Send-SmokeKeySequence -Process $process -Keys @("{UP}") -AutomationId "vfl-crop-x"
            $sentKeyboardStages[$status.stage] = $true
          }
          "keyboard-modal-ready" {
            Invoke-SmokeAutomationElement -Handle $process.MainWindowHandle -Name "About & updates"
            $sentKeyboardStages[$status.stage] = $true
          }
          "keyboard-modal-open" {
            Invoke-SmokeAutomationElement -Handle $process.MainWindowHandle -Name "Close about dialog"
            $sentKeyboardStages[$status.stage] = $true
          }
        }
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
  $observedRequiredStages = @($stageHistory | Where-Object { $requiredStages -contains $_ })
  if (($observedRequiredStages -join "|") -ne ($requiredStages -join "|")) {
    throw ("Portable export smoke required app stages were out of order. Saw: {0}" -f ($stageHistory -join " -> "))
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
  if ($ResizeMode -eq "custom") {
    $videoStream = @($ffprobe.streams | Where-Object { $_.codec_type -eq "video" } | Select-Object -First 1)
    $expectedWidth = [Math]::Max(2, [Math]::Floor($ResizeWidthPx / 2) * 2)
    $expectedHeight = [Math]::Max(2, [Math]::Floor($ResizeHeightPx / 2) * 2)
    if (-not $videoStream -or $videoStream.width -ne $expectedWidth -or $videoStream.height -ne $expectedHeight) {
      $actualWidth = if ($videoStream) { $videoStream.width } else { "none" }
      $actualHeight = if ($videoStream) { $videoStream.height } else { "none" }
      throw ("Portable export smoke output dimensions mismatch. expected={0}x{1} actual={2}x{3}" -f $expectedWidth, $expectedHeight, $actualWidth, $actualHeight)
    }
  }
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
    try {
      & "$env:SystemRoot\System32\taskkill.exe" /PID $process.Id /T /F 2>$null | Out-Null
      $process.Refresh()
    } catch {
      # Fall through to the single-process fallback below.
    }
    if (-not $process.HasExited) {
      Stop-Process -Id $process.Id -Force
    }
  }
}
