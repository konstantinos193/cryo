# Packs cryo into an MSIX — the Microsoft Store artefact, or a locally
# installable dev package. Run from the repo root:
#   powershell -File installer\msix\pack.ps1 -DevSign
#
# The package identity comes from the environment, never an argument —
# the same rule sign.ps1 keeps secrets by, since these strings land in
# release logs:
#
#   CRYO_MSIX_IDENTITY           package identity name, e.g.
#                                12345Publisher.cryo — Partner Center's
#                                "Product identity" panel
#   CRYO_MSIX_PUBLISHER          publisher CN the same panel assigns
#   CRYO_MSIX_PUBLISHER_DISPLAY  shown on the Store page; defaults to
#                                konstantinos193
#   CRYO_MAKEAPPX                explicit makeappx.exe path
#   CRYO_SIGNTOOL                explicit signtool.exe path (dev sign only)
#
# With no identity configured this prints why and exits 0 — like
# sign.ps1, "not configured" is a supported state, not a failure, so the
# release step stays exercised every run instead of waking for the first
# real submission.
#
# -DevSign packs with a development identity, self-signs the package and
#   trusts the cert in CurrentUser\TrustedPeople, so Add-AppxPackage
#   accepts it. Store submission needs none of it — Microsoft re-signs.

[CmdletBinding()]
param(
    [string]$Exe = 'target\release\cryo.exe',
    [string]$Version,
    [string]$Out = 'installer\output\cryo.msix',
    [switch]$DevSign
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

trap {
    [Console]::Error.WriteLine($_.Exception.Message)
    if ($_.InvocationInfo) {
        [Console]::Error.WriteLine("  at $($_.InvocationInfo.ScriptName):$($_.InvocationInfo.ScriptLineNumber)")
    }
    exit 1
}

function Get-Env([string]$name) {
    $v = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($v)) { return $null }
    return $v
}

# Same newest-SDK discovery sign.ps1 uses; two copies is not yet a
# pattern worth extracting (AGENTS: the third copy is).
function Resolve-SdkTool([string]$exe, [string]$envName) {
    $explicit = Get-Env $envName
    if ($explicit) {
        if (-not (Test-Path -LiteralPath $explicit)) {
            throw "$envName points at a file that does not exist: $explicit"
        }
        return (Resolve-Path -LiteralPath $explicit).Path
    }
    $found = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\$exe" `
        -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
    if (-not $found) {
        throw "$exe not found. Install the Windows SDK or set $envName."
    }
    return $found.FullName
}

function Invoke-Native([string]$exe, [string[]]$exeArgs) {
    $ErrorActionPreference = 'Continue'
    $lines = & $exe @exeArgs 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.ToString() } else { "$_" }
    }
    return [pscustomobject]@{ Output = (($lines -join [Environment]::NewLine).TrimEnd()); ExitCode = $LASTEXITCODE }
}

$exePath = (Resolve-Path -LiteralPath $Exe).Path

# Version: the 4-part form MSIX wants. Read Cargo.toml when not given —
# the release workflow passes the tag, which the workflow already proved
# matches it.
if (-not $Version) {
    $cargo = (Select-String -Path 'Cargo.toml' -Pattern '^version = "([^"]+)"').Matches.Groups[1].Value
    $Version = "$cargo.0"
}

$identity = Get-Env 'CRYO_MSIX_IDENTITY'
$publisher = Get-Env 'CRYO_MSIX_PUBLISHER'
if (-not $identity -or -not $publisher) {
    if (-not $DevSign) {
        Write-Output 'No MSIX identity configured; skipping packaging.'
        Write-Output 'Set CRYO_MSIX_IDENTITY + CRYO_MSIX_PUBLISHER (Partner Center product identity) or pass -DevSign for a local package.'
        exit 0
    }
    $identity  = 'cryo.dev'
    $publisher = 'CN=cryo-dev'
}
$display = Get-Env 'CRYO_MSIX_PUBLISHER_DISPLAY'
if (-not $display) { $display = 'konstantinos193' }

$layout = Join-Path ([IO.Path]::GetTempPath()) "cryo-msix-$([guid]::NewGuid().Guid)"
try {
    New-Item -ItemType Directory -Force "$layout\Assets" | Out-Null
    Copy-Item $exePath "$layout\cryo.exe"
    Copy-Item "$PSScriptRoot\assets\*.png" "$layout\Assets\"

    $manifest = [IO.File]::ReadAllText("$PSScriptRoot\Package.appxmanifest")
    foreach ($slot in @('IDENTITY', 'PUBLISHER_DISPLAY', 'PUBLISHER', 'VERSION')) {
        $value = @{ IDENTITY = $identity; PUBLISHER_DISPLAY = $display;
                    PUBLISHER = $publisher; VERSION = $Version }[$slot]
        $manifest = $manifest.Replace("@$slot@", $value)
    }
    [IO.File]::WriteAllText("$layout\AppxManifest.xml", $manifest)

    $makeappx = Resolve-SdkTool 'makeappx.exe' 'CRYO_MAKEAPPX'
    Write-Output "makeappx:  $makeappx"
    Write-Output "identity:  $identity / $publisher"
    Write-Output "version:   $Version"

    $outDir = Split-Path $Out -Parent
    if ($outDir) { New-Item -ItemType Directory -Force $outDir | Out-Null }
    # makeappx gets an absolute path — Join-Path on a rooted $Out would
    # concatenate the two into garbage.
    $outPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
    $r = Invoke-Native $makeappx @('pack', '/d', $layout, '/p', $outPath, '/o')
    Write-Output $r.Output
    if ($r.ExitCode -ne 0) { throw "makeappx failed (exit $($r.ExitCode))" }

    if ($DevSign) {
        $signtool = Resolve-SdkTool 'signtool.exe' 'CRYO_SIGNTOOL'
        # The cert subject must equal the manifest Publisher string —
        # sideloading checks nothing else about it.
        $subject = if ($publisher -like 'CN=*') { $publisher } else { "CN=$publisher" }
        $cert = Get-ChildItem Cert:\CurrentUser\My |
            Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey } | Select-Object -First 1
        if (-not $cert) {
            $cert = New-SelfSignedCertificate -Subject $subject -Type CodeSigningCert `
                -CertStoreLocation 'Cert:\CurrentUser\My' -KeyExportPolicy Exportable
        }
        $r = Invoke-Native $signtool @('sign', '/fd', 'sha256', '/sha1', $cert.Thumbprint, $Out)
        Write-Output $r.Output
        if ($r.ExitCode -ne 0) { throw "signtool failed (exit $($r.ExitCode))" }
        # TrustedPeople takes a self-signed code cert without a prompt —
        # CurrentUser scope, so no elevation and no system-wide trust.
        $cer = Export-Certificate -Cert $cert -Type CERT -FilePath "$layout\dev.cer" -Force
        Import-Certificate -FilePath $cer.FullName -CertStoreLocation 'Cert:\CurrentUser\TrustedPeople' | Out-Null
        Write-Output "dev cert $($cert.Thumbprint) trusted in CurrentUser\TrustedPeople"
        Write-Output "install with:  Add-AppxPackage $Out"
    }
} finally {
    Remove-Item -LiteralPath $layout -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output "packed $Out"
