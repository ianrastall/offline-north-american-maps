# Scripted acceptance check of the installed Offline Maps (Windows PowerShell 5.1).
# -Mode offline waits until this PC has no network, runs the check, tells Ian to reconnect,
# then waits for the network to return. It never changes network settings itself.
param([ValidateSet('online', 'offline')][string]$Mode = 'online', [string]$Out)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class W { [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }
'@

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $env:LOCALAPPDATA 'Programs\Offline Maps\OfflineMaps.exe'
$data = Join-Path $env:LOCALAPPDATA 'Jupiter\OfflineMaps'
if (-not $Out) { $Out = Join-Path $env:TEMP "offline-maps-check-$Mode" }
New-Item -ItemType Directory -Force $Out | Out-Null
$log = Join-Path $Out 'report.txt'
Set-Content $log "Offline Maps check, mode $Mode, started $(Get-Date -Format o)" -Encoding UTF8
function Note($s) { Add-Content $log $s -Encoding UTF8 }

function Online {
    $up = Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.InterfaceDescription -notmatch 'Hyper-V|Loopback' }
    $inet = Get-NetConnectionProfile -ErrorAction SilentlyContinue | Where-Object { $_.IPv4Connectivity -eq 'Internet' -or $_.IPv6Connectivity -eq 'Internet' }
    $tcp = $false
    try { $c = New-Object Net.Sockets.TcpClient; $tcp = $c.ConnectAsync('1.1.1.1', 443).Wait(3000); $c.Close() } catch { $tcp = $false }
    return [pscustomobject]@{ Adapters = @($up | ForEach-Object Name); Internet = [bool]$inet; Tcp = $tcp; Any = ([bool]$up -or [bool]$inet -or $tcp) }
}

if ($Mode -eq 'offline') {
    $deadline = (Get-Date).AddMinutes(30)
    do { $n = Online; if (-not $n.Any) { break }; Start-Sleep 5 } while ((Get-Date) -lt $deadline)
    if ($n.Any) { Note "ABORT: network still available after 30 minutes"; exit 3 }
    Start-Sleep 5
}
$n = Online
Note "Network before launch: adapters up [$($n.Adapters -join ', ')], internet profile $($n.Internet), TCP to 1.1.1.1:443 $($n.Tcp)"

# Keep Ian's view and window settings; the check moves the map.
Copy-Item (Join-Path $data 'settings.json') (Join-Path $Out 'settings.backup.json') -Force
$placesBefore = Get-Content (Join-Path $data 'Maps\saved-places.json') -Raw
$logLinesBefore = @(Get-Content (Join-Path $data 'offline-maps.log') -ErrorAction SilentlyContinue).Count

Get-Process OfflineMaps -ErrorAction SilentlyContinue | ForEach-Object { $_.CloseMainWindow() | Out-Null; $_.WaitForExit(10000) | Out-Null }

$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'
$t0 = Get-Date
$proc = Start-Process $exe -PassThru
Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS

$AE = [System.Windows.Automation.AutomationElement]
$TS = [System.Windows.Automation.TreeScope]
$TW = [System.Windows.Automation.TreeWalker]::RawViewWalker
# A Descendants search stops inside the WebView2, so walk the tree and skip it.
function ById($root, $id) {
    $c = $TW.GetFirstChild($root)
    while ($c) {
        if ($c.Current.AutomationId -eq $id) { return $c }
        if ($c.Current.ClassName -ne 'Microsoft.UI.Xaml.Controls.WebView2') { $f = ById $c $id; if ($f) { return $f } }
        $c = $TW.GetNextSibling($c)
    }
    return $null
}

$win = $null
for ($i = 0; $i -lt 120 -and -not $win; $i++) {
    Start-Sleep -Milliseconds 500; $proc.Refresh()
    if ($proc.MainWindowHandle -ne 0) { $win = $AE::FromHandle($proc.MainWindowHandle) }
}
if (-not $win) { Note "FAIL: no window after 60 s"; exit 4 }
Note "Window '$($win.Current.Name)' after $([int]((Get-Date) - $t0).TotalSeconds) s"

# Wait for the map page.
$ready = $false
for ($i = 0; $i -lt 60 -and -not $ready; $i++) {
    Start-Sleep 1
    try { $j = Invoke-RestMethod http://127.0.0.1:9222/json -TimeoutSec 2; $ready = [bool]($j | Where-Object { $_.url -like 'https://map.local/*' }) } catch {}
}
Note "Map page ready: $ready after $([int]((Get-Date) - $t0).TotalSeconds) s"
Start-Sleep 8

# Remote connections by the app and its WebView2 processes, sampled through the run.
$remote = @{}
function Tree { $all = Get-CimInstance Win32_Process; $ids = @($proc.Id); do { $added = $all | Where-Object { $ids -contains $_.ParentProcessId -and $ids -notcontains $_.ProcessId } | ForEach-Object ProcessId; $ids += $added } while ($added); $ids }
function Sample {
    $ids = Tree
    Get-NetTCPConnection -ErrorAction SilentlyContinue | Where-Object { $ids -contains $_.OwningProcess -and $_.RemoteAddress -notmatch '^(127\.|::1|0\.0\.0\.0|::$)' } | ForEach-Object {
        $p = (Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue).ProcessName
        $remote["$p -> $($_.RemoteAddress):$($_.RemotePort) $($_.State)"] = 1
    }
}
function Cdp($cmd, $file) { & node (Join-Path $here 'cdp.mjs') $cmd (Join-Path $Out $file) 2>&1 | ForEach-Object { Note "  cdp: $_" } }
function Text($id) { $e = ById $win $id; if ($e) { $e.Current.Name } else { '(not found)' } }
function Items {
    $list = ById $win 'Results'
    if (-not $list) { return @('(no list)') }
    $c = New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)
    @($list.FindAll($TS::Children, $c) | ForEach-Object {
        $names = @($_.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name } | Where-Object { $_ })
        if ($names.Count) { $names[0] } else { $_.Current.Name }
    })
}
function ErrorBar { $e = ById $win 'ErrorBar'; if ($e -and -not $e.Current.IsOffscreen -and $e.Current.BoundingRectangle.Height -gt 0) { "shown: " + (@($e.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name } | Where-Object { $_ }) -join ' | ') } else { 'not shown' } }

Sample
Cdp info '01-start.json'; Cdp shot '01-start.png'
Note "Status bar: $(Text 'StatusText') | $(Text 'ViewText')"
Note "Error bar: $(ErrorBar)"
Note "List heading: $(Text 'ListHeading'); items: $((Items) -join ' || ')"

function Search($q, $tag, [switch]$Select) {
    $box = ById $win 'SearchBox'
    $edit = $box.FindFirst($TS::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)))
    [W]::SetForegroundWindow($proc.MainWindowHandle) | Out-Null
    $edit.SetFocus(); Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait('^a{BACKSPACE}')
    [System.Windows.Forms.SendKeys]::SendWait(($q -replace '([+^%~(){}\[\]])', '{$1}'))
    $sw = [Diagnostics.Stopwatch]::StartNew(); Start-Sleep 2
    Note ""
    Note "Search '$q': heading '$(Text 'ListHeading')'; empty text '$(Text 'ListEmpty')'"
    $items = Items
    $k = 0; foreach ($it in ($items | Select-Object -First 8)) { $k++; Note "  $k. $it" }
    Note "  ($($items.Count) items listed)"
    if ($Select -and $items.Count) {
        $list = ById $win 'Results'
        $first = $list.FindFirst($TS::Children, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)))
        $first.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
        Start-Sleep 6; Sample
        Note "  selected first result; view now: $(Text 'ViewText')"
        Cdp shot "$tag.png"
    }
    Sample
}

Search 'springfield' '02-springfield' -Select
Search '1060 w addison st chicago' '03-address' -Select
Search 'fuel' '04-fuel' -Select
Search 'campground' '05-campground' -Select
Search 'drinking water' '06-water'
Search 'charging' '07-charging'

# Back to the saved place.
$box = ById $win 'SearchBox'
$edit = $box.FindFirst($TS::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)))
[W]::SetForegroundWindow($proc.MainWindowHandle) | Out-Null
$edit.SetFocus(); [System.Windows.Forms.SendKeys]::SendWait('^a{BACKSPACE}'); Start-Sleep 2
Note ""
Note "Cleared search: heading '$(Text 'ListHeading')'; items: $((Items) -join ' || ')"
$list = ById $win 'Results'
$first = $list.FindFirst($TS::Children, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)))
if ($first) { $first.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select(); Start-Sleep 6; Note "Selected saved place; view now: $(Text 'ViewText')"; Cdp shot '08-saved-place.png' }

Sample
Cdp info '09-end.json'
Note "Error bar at end: $(ErrorBar)"
$n = Online
Note "Network at end: adapters up [$($n.Adapters -join ', ')], internet profile $($n.Internet), TCP $($n.Tcp)"
Note ""
Note "Remote TCP connections seen from the app's process tree:"
if ($remote.Count) { $remote.Keys | Sort-Object | ForEach-Object { Note "  $_" } } else { Note "  none" }

$proc.CloseMainWindow() | Out-Null; $proc.WaitForExit(15000) | Out-Null
Start-Sleep 2
Copy-Item (Join-Path $Out 'settings.backup.json') (Join-Path $data 'settings.json') -Force
$placesAfter = Get-Content (Join-Path $data 'Maps\saved-places.json') -Raw
Note "Saved places file unchanged: $($placesBefore -eq $placesAfter)"
$newLog = @(Get-Content (Join-Path $data 'offline-maps.log') -ErrorAction SilentlyContinue | Select-Object -Skip $logLinesBefore)
Note "New app log lines: $($newLog.Count)"; $newLog | Select-Object -First 20 | ForEach-Object { Note "  $_" }
Note "Finished $(Get-Date -Format o)"

if ($Mode -eq 'offline') {
    $msg = "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.MessageBox]::Show('The offline check of Offline Maps has finished. You can switch Airplane mode off now.', 'Offline check finished', 'OK', 'Information', 'Button1', 'ServiceNotification')"
    Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile', '-Command', $msg
    [System.Media.SystemSounds]::Asterisk.Play()
    $deadline = (Get-Date).AddMinutes(60)
    do { Start-Sleep 5; $n = Online } while (-not $n.Tcp -and (Get-Date) -lt $deadline)
    Note "Network back: $($n.Tcp) at $(Get-Date -Format o)"
}
