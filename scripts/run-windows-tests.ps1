# Run the cross-built Windows test exes from `just build-tests-windows`.
#
# Usage (from the repo root, in the VM's PowerShell):
#   .\scripts\run-windows-tests.ps1                 # every suite, newest exe per crate
#   .\scripts\run-windows-tests.ps1 jinn_kernel     # only suites matching a filter
#   .\scripts\run-windows-tests.ps1 -List           # just enumerate suites, run nothing
#
# Each exe is a self-contained libtest harness (CRT-static - nothing to
# install). All output is captured to win-test-results.log; the summary at
# the end names every failing suite.

[CmdletBinding()]
param(
    # Substring filter: only run suites whose name contains this (libtest-style).
    [Parameter(Position = 0)]
    [string]$Filter = "",

    # Enumerate the suites without running them.
    [switch]$List
)

$ErrorActionPreference = 'Stop'

$depsDir = Join-Path (Get-Location) 'target\x86_64-pc-windows-msvc\debug\deps'
if (-not (Test-Path $depsDir)) {
    Write-Error "No deps dir at '$depsDir' - run 'just build-tests-windows' on the Linux side first."
}

# One exe per crate, but rebuilds leave stale-hash copies behind. Run only
# the newest exe for each crate name (name is <crate>-<hash>.exe).
$suites = Get-ChildItem -Path $depsDir -Filter '*.exe' |
    Group-Object { $_.Name -replace '-[0-9a-f]+\.exe$', '' } |
    ForEach-Object { $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -First 1 } |
    Sort-Object Name

if ($Filter -ne "") {
    $suites = $suites | Where-Object { $_.BaseName -like "*$Filter*" }
}

if (-not $suites) {
    Write-Error "No test exes found in '$depsDir'$(
        if ($Filter -ne '') { " matching '$Filter'" } else { '' })."
}

if ($List) {
    $suites | ForEach-Object { $_.Name }
    Write-Host "`n$($suites.Count) suite(s). Re-run without -List to execute." -ForegroundColor Cyan
    exit 0
}

$log = Join-Path (Get-Location) 'win-test-results.log'
"==> Windows test run started $(Get-Date -Format 'yyyy-MM-ddTHH:mm:ss')" | Set-Content $log

$failed = @()
$total = 0
foreach ($suite in $suites) {
    "===== $($suite.Name) =====" | Tee-Object $log -Append
    # Windows PowerShell 5.1: a native exe's stderr lines under 2>&1 become
    # ErrorRecords, and EAP 'Stop' turns the first one into a script-killing
    # exception. Relax EAP just around the harness invocation.
    $ErrorActionPreference = 'Continue'
    & $suite.FullName 2>&1 | ForEach-Object { "$_" } | Tee-Object $log -Append
    $ErrorActionPreference = 'Stop'
    $total++
    if ($LASTEXITCODE -ne 0) {
        ">>> FAILED (exit $LASTEXITCODE)" | Tee-Object $log -Append
        $failed += $suite.Name
    }
}

# --- Summary ---
"" | Tee-Object $log -Append
"==> Summary: $total suite(s), $($failed.Count) failed" | Tee-Object $log -Append
"    Full log: $log" | Tee-Object $log -Append

if ($failed) {
    Write-Host "`nFailing suites:" -ForegroundColor Red
    foreach ($f in $failed) { Write-Host "    - $f" -ForegroundColor Red }
    Write-Host "`nDetails: select-string -Path '$log' -Pattern '>>> FAILED' -Context 0,40" -ForegroundColor Yellow
    exit 1
}

Write-Host "`nAll suites passed." -ForegroundColor Green
exit 0
