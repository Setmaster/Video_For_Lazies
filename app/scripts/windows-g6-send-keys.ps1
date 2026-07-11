param(
  [Parameter(Mandatory = $true)][int]$ProcessId,
  [Parameter(Mandatory = $true)][string]$Sequence
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class VflG6KeyboardNative {
  [DllImport("user32.dll")]
  public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);

  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
}
"@

$sendKeyByName = @{
  ENTER = "{ENTER}"
  ESC = "{ESC}"
  LEFT = "{LEFT}"
  RIGHT = "{RIGHT}"
  UP = "{UP}"
  DOWN = "{DOWN}"
  SPACE = " "
  TAB = "{TAB}"
}

$keys = @($Sequence.Split(",", [System.StringSplitOptions]::RemoveEmptyEntries) | ForEach-Object {
  $name = $_.Trim().ToUpperInvariant()
  if (-not $sendKeyByName.ContainsKey($name)) {
    throw "Unsupported G6 smoke key name: $name"
  }
  $sendKeyByName[$name]
})
if ($keys.Count -eq 0) {
  throw "G6 smoke key sequence is empty."
}

$process = Get-Process -Id $ProcessId -ErrorAction Stop
$handle = [IntPtr]::Zero
for ($attempt = 0; $attempt -lt 30; $attempt++) {
  $process.Refresh()
  if ($process.HasExited) {
    throw "G6 smoke app exited before keyboard input."
  }
  $handle = $process.MainWindowHandle
  if ($handle -ne [IntPtr]::Zero) {
    break
  }
  Start-Sleep -Milliseconds 100
}
if ($handle -eq [IntPtr]::Zero) {
  throw "G6 smoke could not find the packaged app window for keyboard input."
}

[void][VflG6KeyboardNative]::ShowWindowAsync($handle, 9)
$foreground = $false
for ($attempt = 0; $attempt -lt 10; $attempt++) {
  if ([VflG6KeyboardNative]::SetForegroundWindow($handle)) {
    $foreground = $true
    break
  }
  Start-Sleep -Milliseconds 100
}
if (-not $foreground) {
  $shell = New-Object -ComObject WScript.Shell
  $foreground = $shell.AppActivate($process.Id)
}
if (-not $foreground) {
  throw "G6 smoke could not foreground the packaged app for keyboard input."
}

Start-Sleep -Milliseconds 180
foreach ($key in $keys) {
  [System.Windows.Forms.SendKeys]::SendWait($key)
  Start-Sleep -Milliseconds 180
}
