# Exercises installer\msix\pack.ps1 end to end: packs a real MSIX with a
# test identity and reads the package back as a zip. Run from the repo
# root:  powershell -File installer\msix\pack.tests.ps1
#
# Everything lands in a temp dir and is deleted after. -MakeAppx
# overrides makeappx.exe discovery, for a machine with no SDK.

[CmdletBinding()]
param([string]$MakeAppx)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$packScript = Join-Path $PSScriptRoot 'pack.ps1'
$work       = Join-Path ([IO.Path]::GetTempPath()) "cryo-pack-tests-$([guid]::NewGuid().Guid)"
$packVars   = @('CRYO_MSIX_IDENTITY', 'CRYO_MSIX_PUBLISHER',
                'CRYO_MSIX_PUBLISHER_DISPLAY', 'CRYO_MAKEAPPX', 'CRYO_SIGNTOOL')

$script:passed = 0
$script:failed = 0

function New-TargetExe([string]$name) {
    $path = Join-Path $work "$name.exe"
    Add-Type -OutputType ConsoleApplication -OutputAssembly $path -TypeDefinition `
        'public class P { public static void Main() { System.Console.WriteLine("cryo test"); } }'
    return $path
}

function Invoke-Pack([hashtable]$vars, [string]$out) {
    foreach ($k in $packVars) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    if ($MakeAppx) { $env:CRYO_MAKEAPPX = $MakeAppx }
    foreach ($k in $vars.Keys) { Set-Item "Env:$k" $vars[$k] }

    $ErrorActionPreference = 'Continue'
    $lines = & powershell -NoProfile -ExecutionPolicy Bypass -File $packScript `
        -Exe (New-TargetExe 'target') -Version '9.9.9.0' -Out $out 2>&1 |
        ForEach-Object {
            if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.ToString() } else { "$_" }
        }
    return [pscustomobject]@{
        Output   = ($lines -join [Environment]::NewLine)
        ExitCode = $LASTEXITCODE
    }
}

function Assert-Case([string]$name, [hashtable]$vars, [int]$expectExit, [string]$expectText) {
    $out = Join-Path $work ("$($script:passed + $script:failed)-$((New-Guid).Guid).msix")
    $r = Invoke-Pack $vars $out

    $problems = @()
    if ($r.ExitCode -ne $expectExit) {
        $problems += "expected exit $expectExit, got $($r.ExitCode)"
    }
    if ($r.Output -notmatch [regex]::Escape($expectText)) {
        $problems += "output did not contain '$expectText'"
    }
    if ($expectExit -eq 0 -and $r.Output -match 'skipping packaging' -and (Test-Path $out)) {
        $problems += 'a skipped pack still wrote a package'
    }
    $script:lastOut = $out

    if ($problems.Count -eq 0) {
        Write-Output "  PASS  $name"
        $script:passed++
    } else {
        Write-Output "  FAIL  $name -- $($problems -join '; ')"
        Write-Output ($r.Output -split "`n" | ForEach-Object { "          $_" })
        $script:failed++
    }
}

New-Item -ItemType Directory -Force $work | Out-Null

try {
    Write-Output "work dir: $work"

    Assert-Case 'no identity configured skips cleanly' `
        @{} 0 'skipping packaging'

    Assert-Case 'half an identity skips too' `
        @{ CRYO_MSIX_IDENTITY = 'cryo.test' } 0 'skipping packaging'

    Assert-Case 'a configured identity packs' `
        @{ CRYO_MSIX_IDENTITY = 'cryo.packtest'
           CRYO_MSIX_PUBLISHER = 'CN=cryo pack test' } 0 'packed'

    # The package is a zip: read the manifest back and check that what
    # made it in is the test identity, not a leftover placeholder or a
    # silently dev-signed default.
    $out = $script:lastOut
    if (Test-Path $out) {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($out)
        try {
            $entries = @($zip.Entries | ForEach-Object { $_.FullName })
            $manifestEntry = $zip.Entries | Where-Object { $_.FullName -eq 'AppxManifest.xml' }
            $reader = New-Object IO.StreamReader($manifestEntry.Open())
            $manifest = $reader.ReadToEnd()
            $reader.Close()
        } finally {
            $zip.Dispose()
        }

        $checks = @(
            @{ Name = 'the package holds the executable'
               Ok   = ($entries -contains 'cryo.exe'); Got = ($entries -join ', ') }
            @{ Name = 'the package holds tile art'
               Ok   = ($entries -contains 'Assets/Square150x150Logo.png')
               Got  = ($entries -join ', ') }
            @{ Name = 'the manifest carries the test identity'
               Ok   = ($manifest -match 'Name="cryo\.packtest"'); Got = 'identity checked' }
            @{ Name = 'the manifest carries the test publisher'
               Ok   = ($manifest -match 'Publisher="CN=cryo pack test"'); Got = 'publisher checked' }
            @{ Name = 'the manifest carries the 4-part version'
               Ok   = ($manifest -match 'Version="9\.9\.9\.0"'); Got = 'version checked' }
            @{ Name = 'no placeholder survived substitution'
               Ok   = ($manifest -notmatch '@[A-Z_]+@'); Got = 'placeholders checked' }
        )
        foreach ($check in $checks) {
            if ($check.Ok) {
                Write-Output "  PASS  $($check.Name)"
                $script:passed++
            } else {
                Write-Output "  FAIL  $($check.Name) -- got '$($check.Got)'"
                $script:failed++
            }
        }
    }
} finally {
    foreach ($k in $packVars) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output ''
Write-Output "$script:passed passed, $script:failed failed"
if ($script:failed -gt 0) { exit 1 }
