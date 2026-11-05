# Run the cross-built Windows test exes from `just build-tests-windows`.
#
# Usage (from the repo root, in the VM's PowerShell):
#   .\scripts\run-windows-tests.ps1                 # every suite
#   .\scripts\run-windows-tests.ps1 jinn_kernel     # only suites matching a filter
#   .\scripts\run-windows-tests.ps1 -List           # just enumerate suites
#
# Behavior (no flags needed, this is just how it runs):
#   - Maps the \\VBOXSVR share to a drive letter and cds there first.
#     PS 5.1 cannot make a UNC path the process cwd, so without this every
#     child exe inherits a stale cwd and cwd-relative tests break silently.
#   - Runs each suite with --test-threads=1. Shell-spawning suites
#     (session-lifecycle, tools, term) run many tokio tests in parallel
#     inside the exe, and concurrent cold bash.exe spawns intermittently
#     wedge on a VM (AV scan of the msys2 loader; the rstest timeout cannot
#     fire because the runtime thread is blocked in the synchronous
#     CreateProcess call). One libtest thread removes the race; on a small
#     VM the wall-time cost is minor.
#   - Caps each suite at 240s wall clock and kills a wedged exe, sweeping
#     its orphaned children (a killed msys2 parent can leave bash alive to
#     poison later suites). libtest's baked-in RSTEST_TIMEOUT bounds a test
#     only while its tokio runtime thread stays responsive; a spawn wedged
#     inside CreateProcess blocks the thread and no userland timer fires,
#     so the kill has to live outside the suite.
#   - Runs the exact suite list `just build-tests-windows` writes to
#     win-test-suites.txt (rebuilds leave stale-hash exes in deps\; the
#     manifest names only the fresh ones). Falls back to the deps glob
#     (every hash-suffixed exe; no name-based dedupe - two crates can ship
#     identically-named test targets) if the manifest is absent.
#   - Sets CARGO_BIN_EXE_jinn to the cross-built jinn.exe so the
#     bin-integration suites (install_cli, overgraph_serve, cli_log_file_path)
#     can spawn the binary; a value you set beforehand wins.
#   - Writes win-test-results.log fresh each run, UTF-8 (Tee-Object on PS 5.1
#     would write UTF-16), and prints a failing-suite summary at the end.

[CmdletBinding()]
param(
    # Substring filter: only run suites whose name contains this.
    [Parameter(Position = 0)]
    [string]$Filter = "",

    # Enumerate the suites without running them.
    [switch]$List
)

# Per-suite wall-clock cap in seconds.
$TimeoutSec = 240

$ErrorActionPreference = 'Stop'

# Stamp every run with the script text's version: over a VBoxSF share the
# guest can serve a stale cached copy of this file, and a confusing run is
# unreadable without knowing WHICH text executed. Bump on every edit.
$ScriptVersion = '2026-11-05.5 raw-process-dotnet'

# --- Resolve the repo root and escape any UNC location -----------------------
#
# Running from \\VBOXSVR\<share>, (Get-Location).ProviderPath is the raw UNC
# path. A UNC path cannot become a process cwd on Windows, so children would
# inherit a wrong one; map the share to a free drive letter and move there.
$repo = (Get-Location).ProviderPath
if ($repo -like '\\*') {
    $share = $repo
    $existing = Get-PSDrive -PSProvider FileSystem |
        Where-Object { $_.DisplayRoot -and $_.DisplayRoot -eq $share } |
        Select-Object -First 1
    if ($existing) {
        $drive = $existing.Name
    } else {
        $drive = 'J'
        while (Get-PSDrive -Name $drive -ErrorAction SilentlyContinue) { $drive = 'K' }
        net use "$($drive):" $share /persistent:no | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Could not map '$share' to ${drive}: - cd to a drive-letter checkout and re-run."
        }
    }
    Set-Location "$drive`:\"
    $repo = "$drive`:\"
    Write-Host "Mapped $share -> $repo"
}

$depsDir = Join-Path $repo 'target\x86_64-pc-windows-msvc\debug\deps'
if (-not (Test-Path $depsDir)) {
    Write-Error "No deps dir at '$depsDir' - run 'just build-tests-windows' on the Linux side first."
}

# --- The jinn binary for bin-integration suites ------------------------------
if (-not $env:CARGO_BIN_EXE_jinn) {
    $env:CARGO_BIN_EXE_jinn = Join-Path $repo 'target\x86_64-pc-windows-msvc\debug\jinn.exe'
}
if (-not (Test-Path $env:CARGO_BIN_EXE_jinn)) {
    Write-Warning "CARGO_BIN_EXE_jinn points at a missing file: $($env:CARGO_BIN_EXE_jinn)"
    Write-Warning 'bin-integration suites (install_cli, overgraph_serve, cli_log_file_path) will fail.'
}

# --- Collect the suite exes --------------------------------------------------
$hashSuffix = '-[0-9a-f]+\.exe$'
$manifest = Join-Path $repo 'win-test-suites.txt'

# .NET ReadAllLines, not Get-Content: encoding-agnostic and LF/CRLF-proof,
# which matters over a mapped VBoxSF share.
$manifestLines = @()
if (Test-Path -LiteralPath $manifest) {
    $manifestLines = [System.IO.File]::ReadAllLines($manifest) |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ -ne '' }
}

$suites = @()
if ($manifestLines.Count -gt 0) {
    # Entries are repo-relative POSIX paths (target/x86_64-pc-windows-msvc/
    # debug/deps/<crate>-<hash>.exe), written by the same build that produced
    # the exes - trust them: no per-entry existence probe (the flakiest
    # operation across a freshly-mapped VBoxSF drive). A missing exe fails
    # loudly when run instead of being silently skipped. Keep only test
    # harnesses (hash-suffixed).
    #
    # Self-heal an absolute-POSIX manifest left by an older producer: anchor
    # on the last 'target/' boundary and keep everything from there on.
    Write-Host "Suite source: win-test-suites.txt ($($manifestLines.Count) entries)"
    $suites = @($manifestLines |
        Where-Object { $_ -match $hashSuffix } |
        ForEach-Object {
            $rel = $_
            if ($rel.StartsWith('/')) {
                $idx = $rel.IndexOf('/target/')
                if ($idx -lt 0) { return }
                $rel = $rel.Substring($idx + 1)
            }
            Join-Path $repo ($rel -replace '/', '\')
        } |
        Sort-Object)
} else {
    if (Test-Path -LiteralPath $manifest) {
        Write-Warning 'win-test-suites.txt exists but read back empty; falling back to the deps glob.'
    }
    # No -Filter here: wildcard enumeration is unreliable across network
    # providers (VBoxSF); filter in the pipeline instead. Every hash-suffixed
    # exe found is run - no dedupe by crate name, because two distinct
    # crates may ship identically-named test targets (e.g. toml_edit_smoke),
    # and a name-based dedupe would silently skip a real suite. A stale exe
    # from an earlier build just re-runs; the manifest path is the precise
    # one, this is the fallback.
    $all = @(Get-ChildItem -LiteralPath $depsDir -ErrorAction SilentlyContinue)
    Write-Host "Suite source: deps glob ($(@($all).Count) entries visible)"
    $suites = @($all |
        Where-Object { $_.Extension -eq '.exe' -and $_.Name -match $hashSuffix } |
        ForEach-Object { $_.FullName } |
        Sort-Object)
}

if ($Filter -ne "") {
    $suites = @($suites | Where-Object { $_ -like "*$Filter*" })
}

if (-not $suites) {
    # Self-diagnostics: one run should explain why collection came back
    # empty instead of leaving a bare "no exes" message.
    $diag = @("Suite collection found nothing. Diagnostics:")
    if (Test-Path -LiteralPath $manifest) {
        $raw = [System.IO.File]::ReadAllLines($manifest)
        $diag += "  win-test-suites.txt: present, $($raw.Count) raw line(s)"
        if ($raw.Count -gt 0) {
            $probe = Join-Path $repo ($raw[0].Trim() -replace '/', '\')
            $diag += "  first entry: $($raw[0].Trim())"
            $diag += "  first entry resolves: $(Test-Path -LiteralPath $probe)"
        }
    } else {
        $diag += "  win-test-suites.txt: absent (did 'just build-tests-windows' run on this checkout?)"
    }
    $plain = @(Get-ChildItem -LiteralPath $depsDir -ErrorAction SilentlyContinue)
    $diag += "  deps entries visible: $(@($plain).Count)"
    $diag += "  deps .exe entries visible: $(@($plain | Where-Object Extension -eq '.exe').Count)"
    Write-Error ($diag -join "`n")
}

if ($List) {
    $suites | ForEach-Object { Split-Path $_ -Leaf }
    Write-Host "`n$($suites.Count) suite(s). Re-run without -List to execute." -ForegroundColor Cyan
    exit 0
}

# --- Fresh UTF-8 log every run ----------------------------------------------
$log = Join-Path $repo 'win-test-results.log'
Remove-Item -Path $log -ErrorAction SilentlyContinue
"==> Windows test run started $(Get-Date -Format 'yyyy-MM-ddTHH:mm:ss') | script: $ScriptVersion" |
    Out-File -FilePath $log -Encoding utf8

$failed = @()
$total = 0

foreach ($suite in $suites) {
    $leaf = Split-Path $suite -Leaf
    Write-Host "`n===== $leaf ====="
    "===== $leaf =====" | Out-File -FilePath $log -Append -Encoding utf8

    # Raw .NET Process, not Start-Process: PS 5.1's Start-Process -PassThru
    # object loses exit information (ExitCode reads back null on PS 5.1 +
    # .NET Framework, whatever wait you use), which scored every passing
    # suite as "FAILED (exit )". Started this way, the handle is acquired
    # at Start() and held, so WaitForExit(ms) + ExitCode are deterministic.
    # Output streams are drained via ReadToEndAsync (started before the
    # wait), so a chatty suite cannot deadlock the pipe and the wait stays
    # timeout-capable.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $suite
    $psi.Arguments = '--test-threads=1'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.WorkingDirectory = $repo
    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    [void]$proc.Start()
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()

    $exited = $proc.WaitForExit($TimeoutSec * 1000)

    if (-not $exited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        # msys2 children can survive the parent kill on Windows; sweep the
        # process tree so a wedged bash does not poison later suites.
        Get-CimInstance Win32_Process -Filter "ParentProcessId=$($proc.Id)" |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        ">>> FAILED (timed out after ${TimeoutSec}s - killed)" | Out-File -FilePath $log -Append -Encoding utf8
        Write-Host ">>> FAILED (timed out after ${TimeoutSec}s - killed)" -ForegroundColor Red
        # Bounded peek at whatever printed before the wedge: a surviving
        # child may still hold the pipe, so never block on the tasks here.
        [void]$outTask.Wait(3000)
        if ($outTask.IsCompleted) { $outTask.Result -split "`r?`n" | ForEach-Object { Write-Host $_ } }
        $failed += $leaf
        $total++
        continue
    }

    $exitCode = $proc.ExitCode
    $outTask.Result -split "`r?`n" | ForEach-Object { Write-Host $_; "$_" | Out-File -FilePath $log -Append -Encoding utf8 }
    $err = $errTask.Result
    if ($err.Trim()) {
        $err -split "`r?`n" | ForEach-Object { Write-Host $_ -ForegroundColor DarkGray; "$_" | Out-File -FilePath $log -Append -Encoding utf8 }
    }

    $total++
    if ($exitCode -ne 0) {
        ">>> FAILED (exit $exitCode)" | Out-File -FilePath $log -Append -Encoding utf8
        Write-Host ">>> FAILED (exit $exitCode)" -ForegroundColor Red
        $failed += $leaf
    }
}

# --- Summary ------------------------------------------------------------------
"==> Summary: $total suite(s), $($failed.Count) failed" | Out-File -FilePath $log -Append -Encoding utf8

if ($failed) {
    Write-Host "`nFailing suites:" -ForegroundColor Red
    foreach ($f in $failed) { Write-Host "    - $f" -ForegroundColor Red }
    Write-Host "`nDetails: select-string -Path '$log' -Pattern '>>> FAILED' -Context 0,40" -ForegroundColor Yellow
    exit 1
}

Write-Host "`nAll $total suites passed." -ForegroundColor Green
exit 0
