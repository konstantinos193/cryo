# Authenticode-signs one PE file, timestamps it, and refuses to pass a
# signature Windows would reject. Run from the repo root:
#   powershell -File installer\sign.ps1 -Path target\release\cryo.exe
#
# The certificate comes from the environment, never from an argument, so a
# password never lands in a shell history or a CI command line:
#
#   CRYO_CERT_BASE64 + CRYO_CERT_PASSWORD   a base64 PFX (see CONTRIBUTING)
#   CRYO_CERT_THUMBPRINT                    a cert already in the user's
#                                           store — hardware token, cloud CSP
#   CRYO_SIGNTOOL                           explicit signtool.exe path
#
# With no certificate configured this prints why and exits 0: unsigned
# builds are a supported state, a broken signature is not.

[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Path
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# An uncaught throw arrives in a CI log wrapped in a script line, a
# CategoryInfo and a FullyQualifiedErrorId that repeats the whole message.
# Every failure here is a sentence worth reading, so print the sentence and
# one location, and keep the exit code.
trap {
    [Console]::Error.WriteLine($_.Exception.Message)
    if ($_.InvocationInfo) {
        [Console]::Error.WriteLine("  at $($_.InvocationInfo.ScriptName):$($_.InvocationInfo.ScriptLineNumber)")
    }
    exit 1
}

# Any one of these can be down without stalling a release; signtool has no
# retry of its own, and an untimestamped signature dies with the cert.
$TimestampUrls = @(
    'http://timestamp.digicert.com'
    'http://timestamp.sectigo.com'
    'http://timestamp.globalsign.com/tsa/r6advanced1'
)

$CodeSigningOid = '1.3.6.1.5.5.7.3.3'

function Get-Env([string]$name) {
    # Treat whitespace-only as absent: a secret that is set but empty is the
    # normal shape of "this fork has no certificate".
    $v = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($v)) { return $null }
    return $v
}

function Resolve-SignTool {
    $explicit = Get-Env 'CRYO_SIGNTOOL'
    if ($explicit) {
        if (-not (Test-Path -LiteralPath $explicit)) {
            throw "CRYO_SIGNTOOL points at a file that does not exist: $explicit"
        }
        return (Resolve-Path -LiteralPath $explicit).Path
    }

    # Newest SDK wins; older signtools predate /tr (RFC 3161) support.
    $found = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" `
        -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
    if (-not $found) {
        throw 'signtool.exe not found. Install the Windows SDK or set CRYO_SIGNTOOL.'
    }
    return $found.FullName
}

# Loads the PFX only to check it, so a bad secret fails here with a sentence
# instead of inside signtool with an error code.
function Assert-UsablePfx([byte[]]$bytes, [string]$password) {
    $cert = $null
    foreach ($flags in @('EphemeralKeySet', 'DefaultKeySet')) {
        try {
            $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 `
                (, $bytes + $password + [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]$flags)
            break
        } catch {
            $loadError = $_
        }
    }
    if (-not $cert) {
        throw "CRYO_CERT_BASE64 did not load as a PFX. Wrong CRYO_CERT_PASSWORD, or the secret is not a PFX: $($loadError.Exception.Message)"
    }

    if (-not $cert.HasPrivateKey) {
        throw 'The certificate in CRYO_CERT_BASE64 has no private key. Export the PFX with the key included.'
    }

    $now = Get-Date
    if ($now -lt $cert.NotBefore) {
        throw "Certificate is not valid until $($cert.NotBefore.ToString('u'))."
    }
    if ($now -gt $cert.NotAfter) {
        throw "Certificate expired $($cert.NotAfter.ToString('u')). Signing with it would ship a broken signature."
    }

    # An absent EKU means unrestricted, which is fine. An EKU that exists and
    # omits code signing means this is the wrong certificate — a TLS cert will
    # sign happily and then fail verification.
    $eku = $cert.Extensions | Where-Object { $_.Oid.Value -eq '2.5.29.37' }
    if ($eku -and ($eku.EnhancedKeyUsages.Value -notcontains $CodeSigningOid)) {
        throw "Certificate is not a code-signing certificate (EKU lacks $CodeSigningOid)."
    }

    Write-Output "  subject:  $($cert.Subject)"
    Write-Output "  issuer:   $($cert.Issuer)"
    Write-Output "  expires:  $($cert.NotAfter.ToString('u'))"
    Write-Output "  sha1:     $($cert.Thumbprint)"
}

# Native stderr, captured, without PowerShell's help. Under
# $ErrorActionPreference='Stop' a redirected native stderr line becomes a
# throwing ErrorRecord, which would abort on the first timestamp failure
# instead of letting the retry below do its job. Continue is scoped to this
# function, so everything else keeps failing fast.
function Invoke-Native([string]$exe, [string[]]$exeArgs) {
    $ErrorActionPreference = 'Continue'
    # Flatten the ErrorRecords to their message text; rendered whole they drag
    # a script line, CategoryInfo and FQID into the log for every stderr line.
    $lines = & $exe @exeArgs 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.ToString() } else { "$_" }
    }
    $code = $LASTEXITCODE
    return [pscustomobject]@{
        Output   = (($lines -join [Environment]::NewLine).TrimEnd())
        ExitCode = $code
    }
}

function Invoke-SignTool([string]$signtool, [string[]]$certArgs, [string]$target) {
    foreach ($url in $TimestampUrls) {
        Write-Output "  timestamp: $url"
        $r = Invoke-Native $signtool (@('sign', '/fd', 'sha256', '/td', 'sha256', '/tr', $url) + $certArgs + $target)
        if ($r.ExitCode -eq 0) { return }
        Write-Output $r.Output
        Write-Warning "signtool failed against $url (exit $($r.ExitCode))"
    }
    throw "Signing failed against every timestamp server: $($TimestampUrls -join ', ')"
}

# The gate that makes this script worth having. signtool sign succeeds for
# certs Windows will not honour; verify /pa applies the policy the OS applies
# on a double-click, so a signature that cannot chain fails the release here
# rather than on a user's desktop.
function Assert-Signature([string]$signtool, [string]$target) {
    $r = Invoke-Native $signtool @('verify', '/pa', '/all', $target)
    Write-Output $r.Output
    if ($r.ExitCode -ne 0) {
        throw @"
Signed $target, but Windows would reject the signature (verify /pa exit $($r.ExitCode)).
A self-signed certificate always fails here: SmartScreen and Authenticode
need a chain to a trusted root, which is the one thing self-signing cannot
provide. Shipping this would be worse than shipping unsigned.
"@
    }
}

$target = (Resolve-Path -LiteralPath $Path).Path

$b64        = Get-Env 'CRYO_CERT_BASE64'
$password   = Get-Env 'CRYO_CERT_PASSWORD'
$thumbprint = Get-Env 'CRYO_CERT_THUMBPRINT'

if (-not $b64 -and -not $thumbprint) {
    Write-Output "No signing certificate configured; leaving $target unsigned."
    Write-Output 'Set CRYO_CERT_BASE64 + CRYO_CERT_PASSWORD or CRYO_CERT_THUMBPRINT to sign.'
    exit 0
}
if ($b64 -and $thumbprint) {
    throw 'Set CRYO_CERT_BASE64 or CRYO_CERT_THUMBPRINT, not both — which key signs must not be ambiguous.'
}

$signtool = Resolve-SignTool
Write-Output "signtool: $signtool"
Write-Output "signing:  $target"

$pfxPath = $null
try {
    if ($b64) {
        if (-not $password) {
            throw 'CRYO_CERT_BASE64 is set without CRYO_CERT_PASSWORD.'
        }
        try {
            # FromBase64String tolerates newlines but not PEM armour, which is
            # the usual way a pasted secret arrives broken.
            $bytes = [Convert]::FromBase64String($b64)
        } catch {
            throw 'CRYO_CERT_BASE64 is not valid base64. Produce it with: [Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx"))'
        }
        Assert-UsablePfx $bytes $password

        # A temp file, because signtool reads the key from disk. Deleted in
        # finally even when signing throws.
        $pfxPath = Join-Path ([IO.Path]::GetTempPath()) "cryo-sign-$([guid]::NewGuid().Guid).pfx"
        [IO.File]::WriteAllBytes($pfxPath, $bytes)
        $certArgs = @('/f', $pfxPath, '/p', $password)
    } else {
        # The certificate dialog shows a thumbprint as spaced lowercase pairs,
        # which is how it gets pasted into a secret. Normalise once and use
        # the normalised form for both the lookup and signtool.
        $sha1 = $thumbprint.Replace(' ', '').Replace("`t", '').ToUpperInvariant()
        $store = Get-ChildItem Cert:\CurrentUser\My, Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
            Where-Object { $_.Thumbprint -eq $sha1 }
        if (-not $store) {
            throw "No certificate with thumbprint $sha1 in CurrentUser\My or LocalMachine\My."
        }
        Write-Output "  subject:  $($store[0].Subject)"
        Write-Output "  expires:  $($store[0].NotAfter.ToString('u'))"
        $certArgs = @('/sha1', $sha1)
    }

    Invoke-SignTool $signtool $certArgs $target
    Assert-Signature $signtool $target
} finally {
    if ($pfxPath -and (Test-Path -LiteralPath $pfxPath)) {
        Remove-Item -LiteralPath $pfxPath -Force
    }
}

Write-Output "Signed and verified $target"
