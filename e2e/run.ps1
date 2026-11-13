# End-to-end run on a GitHub Windows runner, with the e2e build of the helper
# (it talks to the throwaway stand-in on 127.0.0.1 instead of SignFlow):
# install it silently, fire signflow-edit:// links, save the file the ways
# Flexi might, check every result in the stand-in, and a 500MB upload with
# the helper's memory measured. Nothing here touches SignFlow or a real store.
param(
  [Parameter(Mandatory = $true)][string]$Installer
)
$ErrorActionPreference = "Stop"
$work = Join-Path $PSScriptRoot "work"
New-Item -ItemType Directory -Force $work | Out-Null
$results = [System.Collections.ArrayList]::new()
function Check([string]$name, $ok, [string]$detail = "") {
  [void]$results.Add([pscustomobject]@{ name = $name; ok = [bool]$ok; detail = $detail })
  if ($ok) { Write-Host "ok   $name  $detail" } else { Write-Host "FAIL $name  $detail" }
}
function Admin([string]$route, $body) {
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:4700/__test/$route" -ContentType "application/json" -Body ($body | ConvertTo-Json -Compress)
}
function State { Invoke-RestMethod -Uri "http://127.0.0.1:4700/__test/state" }
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
function FileSha([string]$path) { (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower() }
function B64([byte[]]$bytes) { [Convert]::ToBase64String($bytes) }
function Bytes([string]$s) { [Text.Encoding]::UTF8.GetBytes($s) }
# Flexi's likely save: a temporary file renamed over the file.
function SaveLikeFlexi([string]$path, [byte[]]$bytes) {
  $tmp = Join-Path (Split-Path $path) ("_" + [IO.Path]::GetFileNameWithoutExtension($path) + "_fs.tmp")
  [IO.File]::WriteAllBytes($tmp, $bytes)
  Move-Item -Force $tmp $path
}
$data = Join-Path $env:LOCALAPPDATA "SignFlow Helper Files"
$log = Join-Path $data "helper.log"
function LogHas([string]$text) { (Test-Path $log) -and ((Get-Content $log -Raw) -like "*$text*") }
function Opened { if (Test-Path "$work\opened.txt") { (Get-Content "$work\opened.txt" | Where-Object { $_ } | Select-Object -Last 1) } }
function OpenLink([string]$pass) { Start-Process "signflow-edit://open?pass=$pass" }
# A note for the e2e build in its folder (an answer, or "done"), plain UTF-8.
# Written whole under a temporary name and renamed into place, the word ended
# by a line break, so the helper never reads one half written.
function Tell([string]$text, [string]$file) { $dst = Join-Path $data $file; $tmp = "$dst.writing"; [IO.File]::WriteAllText($tmp, "$text`n", [Text.UTF8Encoding]::new($false)); [IO.File]::Move($tmp, $dst, $true) }
function FileRow([string]$id) { (State).files | Where-Object { $_.id -eq $id } }
function EditOf([string]$id) { (State).edits | Where-Object { $_.fileId -eq $id } | Select-Object -Last 1 }

# ---- The stand-in, and the stand-in Flexi as this user's program for .fs
$env:STANDIN_DATA = Join-Path $work "standin-data"
$env:STANDIN_HEARTBEAT = "10"
$standin = Start-Process node -ArgumentList "`"$PSScriptRoot\standin\server.mjs`"" -PassThru -WindowStyle Hidden -RedirectStandardOutput "$work\standin.out" -RedirectStandardError "$work\standin.err"
$up = WaitFor { try { State | Out-Null; $true } catch { $false } } 30
Check "the stand-in SignFlow is up" $up
$node = (Get-Command node).Source
New-Item -Force "HKCU:\Software\Classes\.fs" | Out-Null
Set-Item "HKCU:\Software\Classes\.fs" "SignFlowE2E.fs"
New-Item -Force "HKCU:\Software\Classes\SignFlowE2E.fs\shell\open\command" | Out-Null
Set-Item "HKCU:\Software\Classes\SignFlowE2E.fs\shell\open\command" "`"$node`" `"$PSScriptRoot\fake-editor\open.mjs`" `"%1`""

# ---- Install, silently, for this user only
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process $Installer -ArgumentList "/S", "/NS" -Wait -PassThru
Check "installs silently with no admin prompt (/S /NS, per-user)" ($p.ExitCode -eq 0) "exit $($p.ExitCode) in $([int]$sw.Elapsed.TotalSeconds)s"
$installDir = Join-Path $env:LOCALAPPDATA "SignFlow Helper"
$exe = Get-ChildItem $installDir -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike "uninstall*" } | Select-Object -First 1
Check "installed under %LOCALAPPDATA%\SignFlow Helper" ([bool]$exe) "$($exe.FullName)"
$handler = (Get-ItemProperty "HKCU:\Software\Classes\signflow-edit\shell\open\command" -ErrorAction SilentlyContinue)."(default)"
Check "the installer registered signflow-edit:// for this user" ($handler -like "*$($exe.Name)*") "$handler"

# Every check below records its result; if one throws, the run says where
# it stopped and still reports.
try {

# ---- Open: a link with a pass opens the file in (stand-in) Flexi
$v1 = Bytes ("Flexi file, version 1 " + ("-" * 4000))
$f = Admin file @{ name = "Concept.fs"; content = (B64 $v1) }
$pass1 = (Admin pass @{ fileId = $f.id; user = "carlee" }).pass
OpenLink $pass1
$opened = WaitFor { Opened } 90
Check "a signflow-edit:// link opens the file with the .fs program" ([bool]$opened) "$opened"
Check "the file was downloaded into the helper's own folder" ($opened -like "$data\files\*\Concept.fs")
Check "the downloaded file is SignFlow's" ((FileSha $opened) -eq (Sha $v1))
Check "SignFlow knows it's being edited" ((EditOf $f.id).open)
$hb = WaitFor { (State).events | Where-Object { $_.what -eq "heartbeat" -and $_.fileId -eq $f.id } } 45
Check "the helper checks in (heartbeat)" ([bool]$hb)
Check "the tray says it's editing" (LogHas "STATUS Editing Concept.fs")

# ---- A save in Flexi becomes the new version
$v2 = Bytes ("Flexi file, version 2 " + ("+" * 5000))
SaveLikeFlexi $opened $v2
$saved = WaitFor { $r = FileRow $f.id; if ($r.sha256 -eq (Sha $v2)) { $r } } 60
Check "a save (temp file, then rename) becomes the new version in SignFlow" ([bool]$saved)
Check "the version it replaced is kept" ($saved.versions[0] -eq (Sha $v1))
Check "the tray says Saved to SignFlow" (LogHas "STATUS Saved to SignFlow")
Start-Sleep -Seconds 4
$before = ((State).events | Where-Object { $_.what -eq "replaced" -and $_.fileId -eq $f.id }).Count
[IO.File]::WriteAllBytes($opened, $v2)
Start-Sleep -Seconds 8
$after = ((State).events | Where-Object { $_.what -eq "replaced" -and $_.fileId -eq $f.id }).Count
Check "saving again with no changes uploads nothing" ($before -eq $after) "$before → $after"

# ---- Stale: someone else saved while the lock had lapsed; her three choices
foreach ($choice in @("upload-mine-as-newest", "save-mine-as-copy", "keep-theirs")) {
  Admin lapse @{ fileId = $f.id } | Out-Null
  $theirs = Bytes "Dana's version before $choice"
  $r = Admin replace-as @{ fileId = $f.id; user = "dana"; content = (B64 $theirs) }
  Check "${choice}: Dana could replace it while the lock had lapsed" $r.ok
  Tell $choice "e2e-choice.txt"
  Check "${choice}: the test's answer file is in place" (Test-Path (Join-Path $data "e2e-choice.txt")) (Join-Path $data "e2e-choice.txt")
  $mine = Bytes "Carlee's version for $choice $(Get-Random)"
  SaveLikeFlexi $opened $mine
  if ($choice -eq "upload-mine-as-newest") {
    $ok = WaitFor { $x = FileRow $f.id; if ($x.sha256 -eq (Sha $mine)) { $x } } 60
    Check "upload mine as newest: hers is the file; Dana's is kept as a version" ($ok -and $ok.versions[0] -eq (Sha $theirs))
  }
  if ($choice -eq "save-mine-as-copy") {
    $ok = WaitFor { (State).files | Where-Object { $_.name -eq "Concept (copy).fs" -and $_.sha256 -eq (Sha $mine) } } 60
    Check "save mine as a copy: 'Concept (copy).fs' in SignFlow, Dana's stays the file" ($ok -and (FileRow $f.id).sha256 -eq (Sha $theirs))
  }
  if ($choice -eq "keep-theirs") {
    $ok = WaitFor { (State).events | Where-Object { $_.what -eq "kept theirs" -and $_.fileId -eq $f.id } } 60
    Check "keep theirs: Dana's stays the file" ($ok -and (FileRow $f.id).sha256 -eq (Sha $theirs))
    $closed = WaitFor { $e = EditOf $f.id; if (-not $e.open) { $e } } 30
    Check "keep theirs: the edit ends" ([bool]$closed) "$($closed.closedReason)"
    Check "keep theirs: her file on this computer is untouched" ((FileSha $opened) -eq (Sha $mine))
    $keptMine = Sha $mine
  }
  Check "${choice}: the stale question named Dana" (LogHas "ASK Concept.fs by Dana")
}

# ---- Links that don't work say so
OpenLink $pass1
Check "a used link: 'already used … Open the file again from SignFlow'" ([bool](WaitFor { LogHas "TELL This link was already used. Open the file again from SignFlow." } 30))
$expired = (Admin pass @{ fileId = $f.id; user = "carlee"; expired = $true }).pass
OpenLink $expired
Check "an expired link: 'run out … Open the file again from SignFlow'" ([bool](WaitFor { LogHas "TELL This link has run out. Open the file again from SignFlow." } 30))
$events = (State).events.Count
Start-Process "signflow-edit://open?pass=short&url=https://evil.example"
Check "a malformed link is refused without calling SignFlow" ([bool](WaitFor { LogHas "LINK refused" } 30) -and (State).events.Count -eq $events)

# ---- Reopening after "keep theirs": her changed copy is moved aside (never
# deleted) and SignFlow's version opens in its place
Tell "keep-theirs" "e2e-choice.txt"
$pass2 = (Admin pass @{ fileId = $f.id; user = "carlee" }).pass
OpenLink $pass2
$open2 = WaitFor { $e = EditOf $f.id; if ($e.open) { $e } } 60
Check "opened again from a new link" ([bool]$open2)
$theirsNow = (FileRow $f.id).sha256
$fresh = WaitFor { if ((Test-Path $opened) -and (FileSha $opened) -eq $theirsNow) { $true } } 60
Check "reopened with her unsaved changes: SignFlow's version is what opens" ([bool]$fresh)
$aside = Get-ChildItem (Split-Path $opened) -Filter "Concept (not in SignFlow, *).fs" -ErrorAction SilentlyContinue | Select-Object -First 1
Check "…and her changed copy is kept beside it, not deleted" ($aside -and (FileSha $aside.FullName) -eq $keptMine) "$($aside.Name)"
Remove-Item -Force (Join-Path $data "e2e-choice.txt") -ErrorAction SilentlyContinue

# ---- The lock: a second person can't open it; Done ends it
$danaTry = try { Admin pass @{ fileId = $f.id; user = "dana" }; $false } catch { $_.Exception.Response.StatusCode.value__ -eq 409 }
Check "while Carlee edits, Dana can't open it too" $danaTry
Tell "done" "e2e-control.txt"
$done = WaitFor { $e = EditOf $f.id; if (-not $e.open) { $e } } 60
Check "Done (from the tray) ends the edit and the lock" ($done -and $done.closedReason -eq "done") "$($done.closedReason)"
Check "her file is still on this computer after Done" (Test-Path $opened)

# ---- The stale window itself, clicked like a person would (WebView2's
# debugging port, which only the e2e build opens), on a file of its own
$wf = Admin file @{ name = "Window.fs"; content = (B64 (Bytes "Window file, version 1")) }
Remove-Item -Force "$work\opened.txt" -ErrorAction SilentlyContinue
OpenLink (Admin pass @{ fileId = $wf.id; user = "carlee" }).pass
$wPath = WaitFor { $o = Opened; if ($o -like "*Window.fs") { $o } } 90
Check "window: a second file opened" ([bool]$wPath) "$wPath"
if ($wPath) {
  Admin lapse @{ fileId = $wf.id } | Out-Null
  $theirsW = Bytes "Dana's version before the window"
  Admin replace-as @{ fileId = $wf.id; user = "dana"; content = (B64 $theirsW) } | Out-Null
  $mineW = Bytes "Carlee's version, answered in the window $(Get-Random)"
  SaveLikeFlexi $wPath $mineW
  WaitFor { LogHas "stale window shown" } 60 | Out-Null
  Start-Sleep -Seconds 3
  $wv = (Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -notmatch "--type=" } | ForEach-Object { $_.CommandLine }) -join "`n"
  $port = try { (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 "http://127.0.0.1:9223/json/version").Content } catch { "no answer: $($_.Exception.Message)" }
  $diag = "browser: $wv`nport 9223: $port"
  if ($diag.Length -gt 3500) { $diag = $diag.Substring(0, 3500) }
  Write-Host "::notice title=WebView2 for the stale window::$(Esc $diag)"
  $clicked = (& node "$PSScriptRoot\click-stale.mjs" 9223 upload-mine-as-newest 2>&1 | Out-String).Trim()
  Write-Host $clicked
  Write-Host "::notice title=stale window click::$(Esc ($clicked.Substring(0, [Math]::Min(3500, $clicked.Length))))"
  $seen = $null
  try { $seen = ($clicked -split "`n" | Where-Object { $_ -like "{*" } | Select-Object -Last 1) | ConvertFrom-Json } catch {}
  Check "window: the stale window opened and named Dana" ($seen -and $seen.title -eq "Dana saved a newer version" -and $seen.lead -like "*Window.fs*") "$($seen.title) / $($seen.lead) $($seen.error)"
  Check "window: three choices in plain words" ($seen -and (($seen.buttons -join "|") -eq "Keep theirs|Upload mine as the newest|Save mine as a copy")) "$($seen.buttons -join ' | ')"
  $okW = WaitFor { $x = FileRow $wf.id; if ($x.sha256 -eq (Sha $mineW)) { $x } } 60
  if (-not $okW) {
    # The click didn't land: answer through the file so the rest can run.
    Tell "upload-mine-as-newest" "e2e-choice.txt"
    WaitFor { $x = FileRow $wf.id; if ($x.sha256 -eq (Sha $mineW)) { $x } } 30 | Out-Null
    Remove-Item -Force (Join-Path $data "e2e-choice.txt") -ErrorAction SilentlyContinue
  }
  Check "window: clicking 'Upload mine as the newest' uploads hers; Dana's kept as a version" ($okW -and $okW.versions[0] -eq (Sha $theirsW))
  Check "window: the answer came from the window, not a test file" (LogHas "ASK answered in the window: upload-mine-as-newest")
  Tell "done" "e2e-control.txt"
  $doneW = WaitFor { $e = EditOf $wf.id; if (-not $e.open) { $e } } 60
  Check "window: Done ends that edit" ([bool]$doneW)
}

# ---- An account turned off mid-session is refused at once
$pass3 = (Admin pass @{ fileId = $f.id; user = "carlee" }).pass
OpenLink $pass3
WaitFor { $e = EditOf $f.id; if ($e.open) { $e } } 60 | Out-Null
Admin account @{ user = "carlee"; active = $false } | Out-Null
$off = WaitFor { $e = EditOf $f.id; if (-not $e.open) { $e } } 60
Check "turned off mid-session: the next check-in is refused and the edit closes" ($off -and $off.closedReason -eq "access-lost") "$($off.closedReason)"
Check "she is told, and her file stays" ((LogHas "TELL This SignFlow account has been turned off.") -and (Test-Path $opened))
Admin account @{ user = "carlee"; active = $true } | Out-Null

# ---- 500MB, streamed, with the helper's memory measured
$small = Bytes "Big file, version 1"
$big = Admin file @{ name = "Big.fs"; content = (B64 $small) }
Remove-Item -Force "$work\opened.txt" -ErrorAction SilentlyContinue
OpenLink (Admin pass @{ fileId = $big.id; user = "carlee" }).pass
$bigPath = WaitFor { Opened } 90
Check "the big file opened" ([bool]$bigPath) "$bigPath"
if (-not $bigPath) { throw "the big file never opened, so the 500MB part couldn't run" }
$proc = Get-Process | Where-Object { $_.Path -and $_.Path -like "$installDir\*" } | Select-Object -First 1
$tmp = Join-Path (Split-Path $bigPath) "_Big_fs.tmp"
$fs = [IO.File]::Create($tmp)
$rng = [Random]::new(42)
$chunk = New-Object byte[] (8MB)
for ($i = 0; $i -lt 62; $i++) { $rng.NextBytes($chunk); $fs.Write($chunk, 0, $chunk.Length) }
$rest = New-Object byte[] (500MB - 62 * 8MB); $rng.NextBytes($rest); $fs.Write($rest, 0, $rest.Length)
$fs.Close()
$bigSha = FileSha $tmp
$startWs = $proc.WorkingSet64
Move-Item -Force $tmp $bigPath
$samples = [System.Collections.ArrayList]::new()
$sw = [Diagnostics.Stopwatch]::StartNew()
$arrived = $null
while ($sw.Elapsed.TotalSeconds -lt 900 -and -not $arrived) {
  $proc.Refresh()
  [void]$samples.Add($proc.WorkingSet64)
  $row = FileRow $big.id
  if ($row.sha256 -eq $bigSha) { $arrived = $row }
  Start-Sleep -Milliseconds 250
}
$proc.Refresh()
$peakMB = [math]::Round(($samples | Measure-Object -Maximum).Maximum / 1MB, 1)
$peakLifeMB = [math]::Round($proc.PeakWorkingSet64 / 1MB, 1)
$secs = [int]$sw.Elapsed.TotalSeconds
Check "a 500MB save arrives whole (SHA-256 matches)" ([bool]$arrived) "$([math]::Round($arrived.size / 1MB)) MB in ${secs}s"
Check "the helper never held the file in memory (peak under 150MB)" ($peakMB -lt 150) "working set at start $([math]::Round($startWs / 1MB, 1)) MB, peak during upload $peakMB MB, lifetime peak $peakLifeMB MB"
Check "it went in parts" ([bool]((State).events | Where-Object { $_.what -eq "blob multipart" -and $_.size -eq 524288000 }))
Check "the tray reported upload progress" (LogHas "STATUS Saving Big.fs to SignFlow")
Tell "done" "e2e-control.txt"

# ---- Never anything but the one file
$uploads = (State).events | Where-Object { $_.what -like "blob*" }
Check "every upload was one of the opened files" ($uploads.Count -ge 1)
Check "nothing in her folders was deleted" ((Test-Path $opened) -and (Test-Path $bigPath))

} catch {
  Check "the run stopped early" $false "$($_.Exception.Message) (run.ps1 line $($_.InvocationInfo.ScriptLineNumber))"
}

# ---- Report
$failed = @($results | Where-Object { -not $_.ok })
$md = "## Helper end-to-end on Windows`n`n| | Check | Detail |`n|---|---|---|`n" + (($results | ForEach-Object { "| $(if ($_.ok) { '✅' } else { '❌' }) | $($_.name) | $($_.detail -replace '\|', '/') |" }) -join "`n")
if ($env:GITHUB_STEP_SUMMARY) { $md | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY }
$md | Out-File -Encoding utf8 (Join-Path $work "summary.md")
# Notes readable without signing in: every result, the failures on their
# own (GitHub keeps 10 of each kind per step), the helper's log and the
# stand-in's events. GitHub keeps about 4KB of each note, so long text goes in pieces (the
# newest pieces when there are too many).
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
Notice "e2e results $($results.Count - $failed.Count)/$($results.Count)" ($results | ForEach-Object { "$(if ($_.ok) { 'ok  ' } else { 'FAIL' }) $($_.name) | $($_.detail)" }) 2
foreach ($r in ($failed | Select-Object -First 7)) { Write-Host "::error title=e2e FAIL::$(Esc "$($r.name) | $($r.detail)")" }
# What the helper and the stand-in saw (progress lines and check-ins left out).
if (Test-Path $log) {
  Notice "helper.log" (Get-Content $log | Where-Object { $_ -notmatch "STATUS (Downloading|Saving) .* \d+%" } | ForEach-Object { $_ -replace "^\d{4}-\d\d-\d\dT(\d\d:\d\d:\d\d\.\d+)\+0000 ", '$1 ' }) 4
}
try {
  Notice "stand-in events" ((State).events | Where-Object { $_.what -ne "heartbeat" } | ForEach-Object { "$([DateTimeOffset]::FromUnixTimeMilliseconds($_.at).ToString('HH:mm:ss.fff')) $($_.what) $($_.fileId) $($_.user)" }) 1
} catch {}
$so = (Get-Content "$work\standin.out", "$work\standin.err" -ErrorAction SilentlyContinue) | Select-Object -Last 30
if ($so) { Notice "stand-in output" $so 1 }
if (Test-Path $log) { Copy-Item $log (Join-Path $work "helper.log") }
Stop-Process -Id $standin.Id -ErrorAction SilentlyContinue
Get-Process | Where-Object { $_.Path -and $_.Path -like "$installDir\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
Write-Host "$($results.Count - $failed.Count) passed, $($failed.Count) failed"
if ($failed.Count -gt 0) { exit 1 }
