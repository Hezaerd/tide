param(
    [string]$Exe = (Join-Path $PSScriptRoot '../target/release/tide.exe'),
    [string]$Out = (Join-Path $PSScriptRoot '../target/verify')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Threading;
using System.Runtime.InteropServices;

public static class TideVerify {
    [StructLayout(LayoutKind.Sequential)] struct NotifyIcon { public uint Size; public IntPtr Window; public uint Id; public Guid Guid; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    delegate bool WindowCallback(IntPtr window, IntPtr argument);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string className, string title);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] static extern bool EnumWindows(WindowCallback callback, IntPtr argument);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("shell32.dll")] static extern int Shell_NotifyIconGetRect(ref NotifyIcon icon, out Rect rect);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, int x, int y, uint data, UIntPtr extra);
    [DllImport("user32.dll")] static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);

    public static IntPtr Tray(uint pid) {
        IntPtr window = IntPtr.Zero;
        while ((window = FindWindowEx(new IntPtr(-3), window, "RuneTray", null)) != IntPtr.Zero) {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == pid) return window;
        }
        return IntPtr.Zero;
    }

    public static Rect IconRect(IntPtr window) {
        var icon = new NotifyIcon { Window = window, Id = 1, Size = (uint)Marshal.SizeOf(typeof(NotifyIcon)) };
        Rect rect;
        int result = Shell_NotifyIconGetRect(ref icon, out rect);
        if (result != 0) throw new Exception("Shell_NotifyIconGetRect failed: " + result);
        return rect;
    }

    public static Rect? Popup(uint pid) {
        Rect? found = null;
        EnumWindows((window, _) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner != pid || !IsWindowVisible(window)) return true;
            var title = new StringBuilder(64); GetWindowText(window, title, title.Capacity);
            if (title.ToString() != "Tide") return true;
            Rect rect; if (GetWindowRect(window, out rect)) found = rect;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    public static bool MenuVisible() {
        bool found = false;
        EnumWindows((window, _) => {
            var name = new StringBuilder(64); GetClassName(window, name, name.Capacity);
            if (name.ToString() == "#32768" && IsWindowVisible(window)) { found = true; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static void Click(int x, int y, bool right) {
        SetCursorPos(x, y); Thread.Sleep(80);
        mouse_event(right ? 0x0008u : 0x0002u, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(40);
        mouse_event(right ? 0x0010u : 0x0004u, 0, 0, 0, UIntPtr.Zero);
    }

    public static void Escape() {
        keybd_event(0x1B, 0, 0, UIntPtr.Zero); keybd_event(0x1B, 0, 2, UIntPtr.Zero);
    }
}
"@

[void][TideVerify]::SetThreadDpiAwarenessContext([IntPtr](-4))
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
if (Get-Process tide -ErrorAction SilentlyContinue | Where-Object Path -eq $Exe) { throw "Close the running $Exe first." }

function Start-Tide {
    $process = Start-Process -FilePath $Exe -PassThru
    Start-Sleep -Seconds 4
    if ($process.HasExited) { throw "Tide exited at startup with code $($process.ExitCode)." }
    $process
}

# Windows 11 hides new tray icons in the overflow menu. Pin this build's icon so clicks reach it.
function Set-TrayPinned {
    foreach ($key in Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings') {
        $item = Get-ItemProperty $key.PSPath
        if ($item.ExecutablePath -eq $Exe) {
            if ($item.IsPromoted -eq 1) { return $false }
            Set-ItemProperty $key.PSPath -Name IsPromoted -Value 1 -Type DWord
            Write-Host "Pinned the tray icon of $Exe to the taskbar."
            return $true
        }
    }
    throw "Windows has no tray setting for $Exe yet."
}

function Wait-Popup([bool]$Open, [int]$TimeoutMs = 3000) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.ElapsedMilliseconds -lt $TimeoutMs) {
        if ([bool][TideVerify]::Popup($tide.Id) -eq $Open) { return $clock.ElapsedMilliseconds }
        Start-Sleep -Milliseconds 20
    }
    return $null
}

$checks = [ordered]@{}
$timings = [ordered]@{}
$saved = New-Object TideVerify+Point
[void][TideVerify]::GetCursorPos([ref]$saved)
$tide = Start-Tide
try {
    if (Set-TrayPinned) {
        Stop-Process -Id $tide.Id -Force
        Start-Sleep -Milliseconds 500
        $tide = Start-Tide
    }
    $tray = [TideVerify]::Tray($tide.Id)
    if ($tray -eq [IntPtr]::Zero) { throw 'Tide has no tray window.' }
    $icon = [TideVerify]::IconRect($tray)
    $x = [int](($icon.Left + $icon.Right) / 2)
    $y = [int](($icon.Top + $icon.Bottom) / 2)

    [TideVerify]::Click($x, $y, $false)
    $timings['First open'] = Wait-Popup $true
    $checks['Clicking the tray icon opens the pop-up'] = $null -ne $timings['First open']
    Start-Sleep -Milliseconds 300
    $popup = [TideVerify]::Popup($tide.Id)
    if ($popup) {
        $width = $popup.Right - $popup.Left
        $height = $popup.Bottom - $popup.Top
        $bitmap = New-Object System.Drawing.Bitmap $width, $height
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($popup.Left, $popup.Top, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $Out 'popup.png'))
        $graphics.Dispose(); $bitmap.Dispose()
    }

    [TideVerify]::Click($x, $y, $false); Start-Sleep -Milliseconds 700
    $checks['Clicking the tray icon again closes it'] = -not [TideVerify]::Popup($tide.Id)
    [void](Wait-Popup $false)

    $form = New-Object System.Windows.Forms.Form
    $form.StartPosition = 'Manual'
    $form.Location = New-Object System.Drawing.Point 200, 200
    $form.Size = New-Object System.Drawing.Size 240, 160
    $form.TopMost = $true
    $form.Text = 'Tide verify'
    $form.Show(); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 300
    [TideVerify]::Click($x, $y, $false)
    $timings['Second open'] = Wait-Popup $true
    $openBefore = $null -ne $timings['Second open']
    Start-Sleep -Milliseconds 300
    [TideVerify]::Click(320, 280, $false); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 700
    $checks['Clicking another window closes it'] = $openBefore -and -not [TideVerify]::Popup($tide.Id)
    $form.Close()

    [TideVerify]::Click($x, $y, $false)
    $timings['Third open'] = Wait-Popup $true
    Start-Sleep -Milliseconds 300
    [TideVerify]::Escape(); Start-Sleep -Milliseconds 500
    $checks['Escape closes it'] = -not [TideVerify]::Popup($tide.Id)

    [TideVerify]::Click($x, $y, $true); Start-Sleep -Milliseconds 700
    $checks['Right-clicking the tray icon shows the menu'] = [TideVerify]::MenuVisible()
    [TideVerify]::Escape(); Start-Sleep -Milliseconds 300

    $checks['Tide is still running'] = -not $tide.HasExited
} finally {
    if (-not $tide.HasExited) { Stop-Process -Id $tide.Id -Force }
    [void][TideVerify]::SetCursorPos($saved.X, $saved.Y)
}

$failed = 0
foreach ($check in $checks.GetEnumerator()) {
    if ($check.Value) { $mark = 'pass' } else { $mark = 'FAIL'; $failed++ }
    Write-Host ("{0}  {1}" -f $mark, $check.Key)
}
foreach ($timing in $timings.GetEnumerator()) {
    $value = if ($null -eq $timing.Value) { 'over 3000 ms' } else { "$($timing.Value) ms" }
    Write-Host ("time  {0}: {1}" -f $timing.Key, $value)
}
Write-Host "Pop-up capture: $(Join-Path $Out 'popup.png')"
if ($failed) { exit 1 }
