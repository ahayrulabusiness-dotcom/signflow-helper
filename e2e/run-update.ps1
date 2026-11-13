# End-to-end run of the UPDATE flow on a GitHub Windows runner, after run.ps1:
# the installed e2e build (the version being released) checks a local
# stand-in update address (:4702) that serves the next version, built in this
# run and signed with a key made for this run only. Every failure path first
# (tampered, cut off, no signal, not an installer, disk full), then the real
# update — started while a file is open, so it must wait for Done — then the
# new version opens, saves and uploads. Nothing here touches SignFlow, a real
# store or GitHub.
param(
  [Parameter(Mandatory = $true)][string]$Old,
  [Parameter(Mandatory = $true)][string]$Files,
  [Parameter(Mandatory = $true)][string]$Current,
  [Parameter(Mandatory = $true)][string]$Next
)
$ErrorActionPreference = "Stop"
$work = Join-Path $PSScriptRoot "work"
New-Item -ItemType Directory -Force $work | Out-Null
$results = [System.Collections.ArrayList]::new()
function Check([string]$name, $ok, [string]$detail = "") {
  [void]$results.Add([pscustomobject]@{ name = $name; ok = [bool]$ok; skipped = $false; detail = $detail })
  if ($ok) { Write-Host "ok   $name  $detail" } else { Write-Host "FAIL $name  $detail" }
}
# Something the runner itself can't be set up for: said so, not counted as a pass.
function Skip([string]$name, [string]$why) {
  [void]$results.Add([pscustomobject]@{ name = $name; ok = $true; skipped = $true; detail = $why })
  Write-Host "skip $name  $why"
}
function Admin([string]$route, $body) {
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:4700/__test/$route" -ContentType "application/json" -Body ($body | ConvertTo-Json -Compress)
}
function State { Invoke-RestMethod -Uri "http://127.0.0.1:4700/__test/state" }
function Mode([string]$m) { Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:4702/__mode" -ContentType "application/json" -Body (@{ mode = $m } | ConvertTo-Json -Compress) | Out-Null }
function WaitFor([scriptblock]$cond, [int]$seconds) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $seconds) {
    $r = & $cond
    if ($r) { return $r }
    Start-Sleep -Milliseconds 500
  }
  return $null
}
function Esc([string]$t) { $t.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A") }
function Sha([byte[]]$bytes) { ([Security.Cryptography.SHA256]::Create().ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join "" }
function B64([byte[]]$bytes) { [Convert]::ToBase64String($bytes) }
function Bytes([string]$s) { [Text.Encoding]::UTF8.GetBytes($s) }
function SaveLikeFlexi([string]$path, [byte[]]$bytes) {
  $tmp = Join-Path (Split-Path $path) ("_" + [IO.Path]::GetFileNameWithoutExtension($path) + "_fs.tmp")
  [IO.File]::WriteAllBytes($tmp, $bytes)
  Move-Item -Force $tmp $path
}
$data = Join-Path $env:LOCALAPPDATA "SignFlow Helper Files"
$log = Join-Path $data "helper.log"
$installDir = Join-Path $env:LOCALAPPDATA "SignFlow Helper"
function LogText {
  if (-not (Test-Path $log)) { return "" }
  $fs = [IO.File]::Open($log, "Open", "Read", "ReadWrite")
  try { [IO.StreamReader]::new($fs).ReadToEnd() } finally { $fs.Dispose() }
}
$script:mark = 0
function Mark { $script:mark = (LogText).Length }
function Since { $t = LogText; if ($t.Length -ge $script:mark) { $t.Substring($script:mark) } else { $t } }
function Seen([string]$text) { (Since).Contains($text) }
function WaitLog([string]$text, [int]$seconds) { WaitFor { Seen $text } $seconds }
function Opened { if (Test-Path "$work\opened.txt") { (Get-Content "$work\opened.txt" | Where-Object { $_ } | Select-Object -Last 1) } }
function OpenLink([string]$pass) { Start-Process "signflow-edit://open?pass=$pass" }
# Written whole under a temporary name and renamed into place, the word ended
# by a line break, so the helper never reads one half written.
function Tell([string]$text, [string]$file) { $dst = Join-Path $data $file; $tmp = "$dst.writing"; [IO.File]::WriteAllText($tmp, "$text`n", [Text.UTF8Encoding]::new($false)); [IO.File]::Move($tmp, $dst, $true) }
function Control([string]$text) { Tell $text "e2e-control.txt"; WaitFor { -not (Test-Path (Join-Path $data "e2e-control.txt")) } 15 | Out-Null }
function FileRow([string]$id) { (State).files | Where-Object { $_.id -eq $id } }
function EditOf([string]$id) { (State).edits | Where-Object { $_.fileId -eq $id } | Select-Object -Last 1 }
function Helper { Get-Process | Where-Object { $_.Path -and $_.Path -like "$installDir\*" } | Select-Object -First 1 }
function ExePath { (Get-ChildItem $installDir -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike "uninstall*" } | Select-Object -First 1).FullName }
function ExeVersion { (Get-Item (ExePath)).VersionInfo.ProductVersion }
function RegVersion { (Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\SignFlow Helper" -ErrorAction SilentlyContinue).DisplayVersion }
function StopHelper { Get-Process | Where-Object { $_.Path -and $_.Path -like "$installDir\*" } | Stop-Process -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2 }
function StartHelper { Start-Process (ExePath); WaitLog "start: SignFlow Helper" 30 | Out-Null }
# The update window, read (and a button clicked) through WebView2's debugging port.
function Window([string]$action = "read", [string]$waitFor = "") {
  $out = (& node "$PSScriptRoot\click-update.mjs" 9223 $action $waitFor 2>&1 | Out-String).Trim()
  try { ($out -split "`n" | Where-Object { $_ -like "{*" } | Select-Object -Last 1) | ConvertFrom-Json } catch { [pscustomobject]@{ error = $out } }
}
function HerFiles {
  $all = @{}
  if (Test-Path "$data\files") {
    Get-ChildItem "$data\files" -Recurse -File | ForEach-Object { $all[$_.FullName] = (Get-FileHash -Algorithm SHA256 $_.FullName).Hash }
  }
  $all
}
function Shortcuts { @([Environment]::GetFolderPath("Desktop"), [Environment]::GetFolderPath("CommonDesktopDirectory")) | ForEach-Object { Get-ChildItem $_ -Filter *.lnk -ErrorAction SilentlyContinue } | ForEach-Object { $_.FullName } | Sort-Object }

# ---- The stand-ins: SignFlow (:4700/:4701) and the update address (:4702)
$env:STANDIN_DATA = Join-Path $work "standin-data-update"
$env:STANDIN_HEARTBEAT = "10"
$standin = Start-Process node -ArgumentList "`"$PSScriptRoot\standin\server.mjs`"" -PassThru -WindowStyle Hidden -RedirectStandardOutput "$work\standin-update.out" -RedirectStandardError "$work\standin-update.err"
$env:UPDATE_FILES = (Resolve-Path $Files).Path
$env:UPDATE_VERSION = $Next
$env:UPDATE_CURRENT = $Current
$env:UPDATE_MODE = "none"
$updates = Start-Process node -ArgumentList "`"$PSScriptRoot\standin\update-server.mjs`"" -PassThru -WindowStyle Hidden -RedirectStandardOutput "$work\update-server.out" -RedirectStandardError "$work\update-server.err"
Check "the stand-ins are up" (WaitFor { try { State | Out-Null; Invoke-RestMethod "http://127.0.0.1:4702/latest.json" | Out-Null; $true } catch { $false } } 30)
$node = (Get-Command node).Source
New-Item -Force "HKCU:\Software\Classes\.fs" | Out-Null
Set-Item "HKCU:\Software\Classes\.fs" "SignFlowE2E.fs"
New-Item -Force "HKCU:\Software\Classes\SignFlowE2E.fs\shell\open\command" | Out-Null
Set-Item "HKCU:\Software\Classes\SignFlowE2E.fs\shell\open\command" "`"$node`" `"$PSScriptRoot\fake-editor\open.mjs`" `"%1`""

try {

# ---- The installed version, fresh
StopHelper
$p = Start-Process $Old -ArgumentList "/S", "/NS" -Wait -PassThru
Check "the version being released ($Current) is installed" ($p.ExitCode -eq 0 -and (ExeVersion) -like "$Current*" -and (RegVersion) -eq $Current) "exe $(ExeVersion), registry $(RegVersion)"
$herBefore = HerFiles
$shortcutsBefore = (Shortcuts) -join "|"
Mark
StartHelper
Check "it starts as $Current" (Seen "start: SignFlow Helper $Current (e2e build)")
Check "it checks for an update by itself a few seconds after starting" ([bool](WaitLog "UPDATE check (automatic)" 40))
Check "nothing newer: up to date, quietly (no window)" ([bool](WaitLog "UPDATE up to date" 30) -and -not (Seen "UPDATE window shown"))
Check "the menu says so" (Seen "UPDATE menu: Up to date (version $Current)")

# ---- A newer version: the window, once; "Not now"; the dot and the menu stay
Mode "ok"
Mark
Control "check-auto"
Check "a newer version found by the automatic check" ([bool](WaitLog "UPDATE found $Next" 30))
Check "…opens the update window by itself" ([bool](WaitLog "UPDATE window shown" 30))
$w = Window "read" "is available"
Check "window: 'SignFlow Edit $Next is available'" ($w.title -eq "SignFlow Edit $Next is available") "$($w.title) $($w.error)"
Check "window: short release notes from latest.json" ($w.notes -like "Test notes for $Next*") "$($w.notes)"
Check "window: Not now and Update" (($w.buttons -join "|") -eq "Not now=later|Update=update") "$($w.buttons -join ' | ')"
Check "tray: the dot is on" (Seen "UPDATE dot on")
Check "menu: 'Update to $Next' and 'Check for updates now'" (Seen "UPDATE menu: Update to $Next")
$w = Window "later"
Check "Not now: clicked in the window" ([bool](WaitLog "UPDATE window: later clicked" 20)) "$($w.clickError)"
Check "…and the window closes" ([bool](WaitLog "UPDATE window closed" 20))
$saved = WaitFor { try { Get-Content (Join-Path $data "update.json") -Raw | ConvertFrom-Json } catch { $null } } 10
$hours = if ($saved) { ([DateTimeOffset]::FromUnixTimeSeconds([long]$saved.snoozedUntil) - [DateTimeOffset]::UtcNow).TotalHours } else { 0 }
Check "Not now snoozes for 24 hours (update.json)" ($hours -gt 23.9 -and $hours -le 24.0) "$([math]::Round($hours, 3)) hours"
Mark
Control "check-auto"
WaitLog "UPDATE found $Next" 30 | Out-Null
Check "snoozed: the next automatic check shows no window" ([bool](WaitLog "no window (she said Not now)" 15) -and -not (Seen "UPDATE window shown"))
Check "…and the dot stays on" (-not (Seen "UPDATE dot off"))
Mark
Control "check"
Check "Check for updates now shows it even while snoozed" ([bool](WaitLog "UPDATE window shown" 30))
Mark
Window "later" | Out-Null
WaitLog "UPDATE window closed" 20 | Out-Null

# ---- Every failure says what happened and leaves $Current working
$pid0 = (Helper).Id
foreach ($case in @(
    @{ mode = "tampered"; failure = "bad signature"; words = "The update didn't pass its safety check" },
    @{ mode = "cut"; failure = "interrupted"; words = "The download stopped before it finished" },
    @{ mode = "dead"; failure = "no signal"; words = "Couldn't reach the internet" },
    @{ mode = "garbage"; failure = "install failed"; words = "The update couldn't be installed" }
  )) {
  Mode $case.mode
  Mark
  Control "check"
  WaitLog "UPDATE found $Next" 30 | Out-Null
  Control "update"
  $failed = WaitLog "UPDATE failed ($($case.failure))" 120
  $said = WaitLog "UPDATE window says: SignFlow Edit $Next wasn't installed | $($case.words)" 20
  $w = Window "read" "wasn't installed"
  Check "$($case.mode): '$($case.failure)', said in the window" ($failed -and $said -and $w.message -like "*$($case.words)*" -and $w.message -like "*Nothing was changed: SignFlow Edit $Current still works.*") "$($w.message) $($w.error)"
  Check "$($case.mode): Try again and Close" (($w.buttons -join "|") -eq "Close=close|Try again=retry") "$($w.buttons -join ' | ')"
  Check "$($case.mode): $Current still installed and running (same process)" ((Helper).Id -eq $pid0 -and (ExeVersion) -like "$Current*" -and (RegVersion) -eq $Current) "pid $((Helper).Id) vs $pid0, exe $(ExeVersion)"
  Mark
  Window "close" | Out-Null
  WaitLog "UPDATE window closed" 20 | Out-Null
}
$saved = Get-Content (Join-Path $data "update.json") -Raw | ConvertFrom-Json
Check "no failed try left an install half-recorded" (-not $saved.installing)

# ---- Disk full: the helper's temporary folder on a tiny disk that's full
$vhd = Join-Path $work "tiny.vhdx"
$made = $false
try {
  "create vdisk file=`"$vhd`" maximum=24 type=fixed`nselect vdisk file=`"$vhd`"`nattach vdisk`ncreate partition primary`nformat fs=ntfs quick label=TINY`nassign letter=T" | Set-Content -Encoding ascii "$work\dp-make.txt"
  $dp = (diskpart /s "$work\dp-make.txt" 2>&1 | Out-String)
  $made = Test-Path "T:\"
  if (-not $made) { throw "no T: drive: $dp" }
  $free = [IO.DriveInfo]::new("T").AvailableFreeSpace
  fsutil file createnew "T:\fill.bin" ($free - 400KB) | Out-Null
  New-Item -ItemType Directory -Force "T:\tmp" -ErrorAction SilentlyContinue | Out-Null
} catch {
  Skip "disk full: the update says so and $Current keeps working" "couldn't make a tiny disk on the runner: $($_.Exception.Message)"
}
if ($made) {
  StopHelper
  Mark
  $oldTemp = $env:TEMP; $oldTmp = $env:TMP
  $env:TEMP = "T:\tmp"; $env:TMP = "T:\tmp"
  StartHelper
  $env:TEMP = $oldTemp; $env:TMP = $oldTmp
  $pidT = (Helper).Id
  Mode "ok"
  Control "check"
  WaitLog "UPDATE found $Next" 30 | Out-Null
  Control "update"
  $failed = WaitLog "UPDATE failed (disk full)" 120
  $w = Window "read" "wasn't installed"
  Check "disk full: says there isn't enough free space" ($failed -and $w.message -like "There isn't enough free space on this computer for the update.*") "$($w.message) / free on T: $([IO.DriveInfo]::new('T').AvailableFreeSpace) / $((Since) -split "`n" | Where-Object { $_ -like '*UPDATE failed*' } | Select-Object -Last 1)"
  Check "disk full: $Current still installed and running" ((Helper).Id -eq $pidT -and (ExeVersion) -like "$Current*")
  StopHelper
  "select vdisk file=`"$vhd`"`ndetach vdisk" | Set-Content -Encoding ascii "$work\dp-drop.txt"
  diskpart /s "$work\dp-drop.txt" | Out-Null
}

# ---- The update itself, asked for while a file is open: it waits for Done
StopHelper
Mode "ok"
Mark
StartHelper
# Its own check at start finds it (no window: she said Not now earlier).
WaitLog "UPDATE found $Next" 40 | Out-Null
$uf = Admin file @{ name = "Update.fs"; content = (B64 (Bytes "Update file, version 1")) }
Remove-Item -Force "$work\opened.txt" -ErrorAction SilentlyContinue
OpenLink (Admin pass @{ fileId = $uf.id; user = "carlee" }).pass
$uPath = WaitFor { $o = Opened; if ($o -like "*Update.fs") { $o } } 90
Check "a file is open for editing" ([bool]$uPath -and (EditOf $uf.id).open) "$uPath"
Control "check"
Check "while editing, 'Check for updates now' still shows the window" ([bool](WaitLog "UPDATE window shown" 30))
$pid0 = (Helper).Id
$w = Window "update" "is available"
Check "Update clicked in the window" ([bool](WaitLog "UPDATE window: update clicked" 20)) "$($w.clickError)"
Check "it downloads, with progress in the menu" ([bool](WaitLog "UPDATE downloading $Next" 20) -and [bool](WaitLog "UPDATE menu: Downloading $Next" 60))
Check "the download's signature checks out (this run's key)" ([bool](WaitLog "the signature checks out" 120))
Check "she's editing, so it waits" ([bool](WaitLog "UPDATE waiting for the edit to end before installing" 20))
$w = Window "read" "Finish your edit first"
Check "window: 'Finish your edit first'" ($w.title -eq "Finish your edit first" -and $w.message -like "*click Done*") "$($w.title) / $($w.message)"
Start-Sleep -Seconds 12
Check "never installs while she's editing ($Current, same process, 12 seconds on)" ((Helper).Id -eq $pid0 -and (ExeVersion) -like "$Current*" -and -not (Seen "UPDATE installing"))
$mineU = Bytes "Update file, saved while the update waits $(Get-Random)"
SaveLikeFlexi $uPath $mineU
Check "editing goes on as normal: a save reaches SignFlow while the update waits" ([bool](WaitFor { $r = FileRow $uf.id; if ($r.sha256 -eq (Sha $mineU)) { $r } } 60))
Control "done"
Check "Done ends the edit" ([bool](WaitFor { $e = EditOf $uf.id; if (-not $e.open) { $e } } 60))
Check "…then it installs" ([bool](WaitLog "UPDATE the edit ended; installing now" 30) -and [bool](WaitLog "UPDATE installing $Next (from $Current)" 30))
$gone = WaitFor { -not (Get-Process -Id $pid0 -ErrorAction SilentlyContinue) } 60
Check "the old helper exits" ([bool]$gone)
$back = WaitFor { $h = Helper; if ($h -and $h.Id -ne $pid0) { $h } } 180
Check "the installer starts the helper again by itself" ([bool]$back) "pid $($back.Id)"
Check "it is $Next now" ([bool](WaitLog "start: SignFlow Helper $Next (e2e build)" 60) -and (ExeVersion) -like "$Next*" -and (RegVersion) -eq $Next) "exe $(ExeVersion), registry $(RegVersion)"
Check "it knows it was updated" ([bool](WaitLog "UPDATE updated from $Current to $Next" 30) -and (Seen "UPDATE menu: Updated to version $Next"))
$cmd = if ($back) { (Get-CimInstance Win32_Process -Filter "ProcessId = $($back.Id)").CommandLine } else { "" }
Check "started with no old link in its arguments" ($back -and $cmd -notmatch "signflow-edit:") "$cmd"
Check "still installed for this user only, in the same place" ((ExePath) -like "$installDir\*")
Start-Sleep -Seconds 15
Check "no 'link already used' message after the restart" (-not (Seen "TELL This link was already used"))
Check "its own first check: nothing newer than $Next" ([bool](WaitLog "UPDATE up to date" 30))
$saved = Get-Content (Join-Path $data "update.json") -Raw | ConvertFrom-Json
Check "the install record is cleared" (-not $saved.installing)

# ---- And it still works: open, save, upload on $Next
$af = Admin file @{ name = "After.fs"; content = (B64 (Bytes "After the update, version 1")) }
Remove-Item -Force "$work\opened.txt" -ErrorAction SilentlyContinue
OpenLink (Admin pass @{ fileId = $af.id; user = "carlee" }).pass
$aPath = WaitFor { $o = Opened; if ($o -like "*After.fs") { $o } } 90
Check "$($Next): a link opens the file" ([bool]$aPath) "$aPath"
$mineA = Bytes "After the update, saved $(Get-Random)"
if ($aPath) { SaveLikeFlexi $aPath $mineA }
Check "$($Next): a save is uploaded as the new version" ([bool](WaitFor { $r = FileRow $af.id; if ($r.sha256 -eq (Sha $mineA)) { $r } } 60))
Control "done"
Check "$($Next): Done ends the edit" ([bool](WaitFor { $e = EditOf $af.id; if (-not $e.open) { $e } } 60))

# ---- Her files and her desktop
$herAfter = HerFiles
$changed = @($herBefore.Keys | Where-Object { $herAfter[$_] -ne $herBefore[$_] })
Check "every file she had is still there, unchanged ($($herBefore.Count) files)" ($herBefore.Count -gt 0 -and $changed.Count -eq 0) "$($changed -join ', ')"
Check "no new shortcut on the desktop" (((Shortcuts) -join "|") -eq $shortcutsBefore) "$((Shortcuts) -join ', ')"

} catch {
  Check "the run stopped early" $false "$($_.Exception.Message) (run-update.ps1 line $($_.InvocationInfo.ScriptLineNumber))"
}

# ---- Report
$failed = @($results | Where-Object { -not $_.ok })
$skipped = @($results | Where-Object { $_.skipped })
$passed = $results.Count - $failed.Count - $skipped.Count
$md = "## Helper update end-to-end on Windows`n`n| | Check | Detail |`n|---|---|---|`n" + (($results | ForEach-Object { "| $(if ($_.skipped) { '⏭️' } elseif ($_.ok) { '✅' } else { '❌' }) | $($_.name) | $($_.detail -replace '\|', '/') |" }) -join "`n")
if ($env:GITHUB_STEP_SUMMARY) { $md | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY }
$md | Out-File -Encoding utf8 (Join-Path $work "summary-update.md")
function Notice([string]$title, [string[]]$lines, [int]$max) {
  $chunks = [System.Collections.ArrayList]::new(); $cur = ""
  foreach ($l in $lines) {
    if (($cur.Length + $l.Length + 1) -gt 3600 -and $cur) { [void]$chunks.Add($cur); $cur = "" }
    $cur += "$l`n"
  }
  if ($cur) { [void]$chunks.Add($cur) }
  $from = [Math]::Max(0, $chunks.Count - $max)
  for ($i = $from; $i -lt $chunks.Count; $i++) { Write-Host "::notice title=$title ($($i + 1)/$($chunks.Count))::$(Esc $chunks[$i].TrimEnd())" }
}
Notice "update e2e $passed passed, $($failed.Count) failed, $($skipped.Count) skipped" ($results | ForEach-Object { "$(if ($_.skipped) { 'skip' } elseif ($_.ok) { 'ok  ' } else { 'FAIL' }) $($_.name) | $($_.detail)" }) 3
foreach ($r in ($failed | Select-Object -First 7)) { Write-Host "::error title=update e2e FAIL::$(Esc "$($r.name) | $($r.detail)")" }
if (Test-Path $log) {
  Notice "helper.log (updates)" ((LogText) -split "`n" | Where-Object { $_ -match "UPDATE|start:|TELL|LINK|SESSION|e2e:" -and $_ -notmatch "UPDATE menu: Downloading .* (1|2|3|4|6|7|8|9)\d?%" } | ForEach-Object { $_ -replace "^\d{4}-\d\d-\d\dT(\d\d:\d\d:\d\d\.\d+)\+0000 ", '$1 ' } | Select-Object -Last 160) 4
  Copy-Item $log (Join-Path $work "helper-update.log")
}
$so = (Get-Content "$work\update-server.out", "$work\update-server.err" -ErrorAction SilentlyContinue) | Select-Object -Last 20
if ($so) { Notice "update stand-in" $so 1 }
Stop-Process -Id $standin.Id -ErrorAction SilentlyContinue
Stop-Process -Id $updates.Id -ErrorAction SilentlyContinue
StopHelper
Write-Host "$passed passed, $($failed.Count) failed, $($skipped.Count) skipped"
if ($failed.Count -gt 0) { exit 1 }
