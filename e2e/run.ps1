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
  Set-Content -NoNewline (Join-Path $data "e2e-choice.txt") $choice
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

# ---- The stale window itself, clicked like a person would (WebView2's
# debugging port, which only the e2e build opens)
Remove-Item -Force (Join-Path $data "e2e-choice.txt") -ErrorAction SilentlyContinue
$passW = (Admin pass @{ fileId = $f.id; user = "carlee" }).pass
OpenLink $passW
$openW = WaitFor { $e = EditOf $f.id; if ($e.open) { $e } } 60
Check "window: opened again (her kept-theirs copy moved aside first)" ([bool]$openW)
Admin lapse @{ fileId = $f.id } | Out-Null
$theirsW = Bytes "Dana's version before the window"
Admin replace-as @{ fileId = $f.id; user = "dana"; content = (B64 $theirsW) } | Out-Null
$mineW = Bytes "Carlee's version, answered in the window $(Get-Random)"
SaveLikeFlexi $opened $mineW
$clicked = & node "$PSScriptRoot\click-stale.mjs" 9223 upload-mine-as-newest 2>&1 | Out-String
Write-Host $clicked
$seen = $null
try { $seen = ($clicked.Trim() -split "`n" | Where-Object { $_ -like "{*" } | Select-Object -Last 1) | ConvertFrom-Json } catch {}
Check "window: the stale window opened and named Dana" ($seen -and $seen.title -eq "Dana saved a newer version" -and $seen.lead -like "*Concept.fs*") "$($seen.title) / $($seen.lead)"
Check "window: three choices in plain words" ($seen -and (($seen.buttons -join "|") -eq "Keep theirs|Upload mine as the newest|Save mine as a copy")) "$($seen.buttons -join ' | ')"
$okW = WaitFor { $x = FileRow $f.id; if ($x.sha256 -eq (Sha $mineW)) { $x } } 60
Check "window: clicking 'Upload mine as the newest' uploads hers; Dana's kept as a version" ($okW -and $okW.versions[0] -eq (Sha $theirsW))
Check "window: the answer came from the window, not a test file" (LogHas "ASK answered in the window: upload-mine-as-newest")
Set-Content -NoNewline (Join-Path $data "e2e-control.txt") "done"
$doneW = WaitFor { $e = EditOf $f.id; if (-not $e.open) { $e } } 60
Check "window: Done ends that edit" ([bool]$doneW)

# ---- The lock: a second person can't open it; Done ends it
$pass2 = (Admin pass @{ fileId = $f.id; user = "carlee" }).pass
OpenLink $pass2
$open2 = WaitFor { $e = EditOf $f.id; if ($e.open) { $e } } 60
Check "opened again from a new link" ([bool]$open2)
$danaTry = try { Admin pass @{ fileId = $f.id; user = "dana" }; $false } catch { $_.Exception.Response.StatusCode.value__ -eq 409 }
Check "while Carlee edits, Dana can't open it too" $danaTry
Set-Content -NoNewline (Join-Path $data "e2e-control.txt") "done"
$done = WaitFor { $e = EditOf $f.id; if (-not $e.open) { $e } } 60
Check "Done ends the edit and the lock" ($done -and $done.closedReason -eq "done") "$($done.closedReason)"
Check "her file is still on this computer after Done" (Test-Path $opened)

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
Set-Content -NoNewline (Join-Path $data "e2e-control.txt") "done"

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
# One annotation with every result (readable without signing in), and the
# failures on their own (GitHub keeps 10 of each kind per step).
function Esc([string]$t) { $t.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A") }
$lines = ($results | ForEach-Object { "$(if ($_.ok) { 'ok  ' } else { 'FAIL' }) $($_.name) | $($_.detail)" }) -join "`n"
Write-Host "::notice title=e2e results ($($results.Count - $failed.Count)/$($results.Count))::$(Esc $lines)"
foreach ($r in ($failed | Select-Object -First 7)) { Write-Host "::error title=e2e FAIL::$(Esc "$($r.name) | $($r.detail)")" }
# What the helper and the stand-in saw (progress lines left out), readable
# without signing in.
if (Test-Path $log) {
  $hl = (Get-Content $log | Where-Object { $_ -notmatch "STATUS (Downloading|Saving) .* \d+%" } | Select-Object -Last 150) -join "`n"
  Write-Host "::notice title=helper.log (last 150 lines)::$(Esc $hl)"
}
try {
  $ev = ((State).events | ForEach-Object { "$([DateTimeOffset]::FromUnixTimeMilliseconds($_.at).ToString('HH:mm:ss.fff')) $($_.what) $($_.fileId) $($_.user)" } | Select-Object -Last 120) -join "`n"
  Write-Host "::notice title=stand-in events (last 120)::$(Esc $ev)"
} catch {}
$so = ((Get-Content "$work\standin.out", "$work\standin.err" -ErrorAction SilentlyContinue) | Select-Object -Last 40) -join "`n"
if ($so) { Write-Host "::notice title=stand-in output::$(Esc $so)" }
if (Test-Path $log) { Copy-Item $log (Join-Path $work "helper.log") }
Stop-Process -Id $standin.Id -ErrorAction SilentlyContinue
Get-Process | Where-Object { $_.Path -and $_.Path -like "$installDir\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
Write-Host "$($results.Count - $failed.Count) passed, $($failed.Count) failed"
if ($failed.Count -gt 0) { exit 1 }
