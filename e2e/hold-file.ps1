# A stand-in for Flexi keeping the file open while it is edited, and writing
# it only when it closes: holds the file open (others may still read and
# write), waits for <Signal> to appear, then writes <NewContent> into it (if
# given) and closes it. Windows runner only.
#   pwsh -File e2e/hold-file.ps1 -Path <file> -Signal <file> [-NewContent <file>]
param(
  [Parameter(Mandatory = $true)][string]$Path,
  [Parameter(Mandatory = $true)][string]$Signal,
  [string]$NewContent = ""
)
$s = [IO.File]::Open($Path, "Open", "ReadWrite", "ReadWrite")
try {
  while (-not (Test-Path $Signal)) { Start-Sleep -Milliseconds 200 }
  if ($NewContent -and (Test-Path $NewContent)) {
    $b = [IO.File]::ReadAllBytes($NewContent)
    $s.SetLength(0)
    $s.Write($b, 0, $b.Length)
    $s.Flush()
  }
} finally {
  $s.Close()
}
