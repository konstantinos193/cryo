# Exercises installer\sign.ps1 end to end against self-signed certificates,
# so the release signing path is proven without a real certificate. Run from
# the repo root:
#   powershell -File installer\sign.tests.ps1
#
# Certificates and signed copies are made in a temp directory and deleted
# afterwards. Nothing here writes to a trust store: adding a root blocks on an
# invisible confirmation dialog in a non-interactive process, which hangs the
# run rather than failing it. The cost is that sign.ps1's own success tail
# cannot be reached — a self-signed chain can never satisfy `verify /pa` — so
# the signature is instead read back off the file, and the accepting half of
# the gate is checked against a binary Microsoft already signed.
#
# -SignTool overrides signtool.exe discovery, for a machine with no SDK.

[CmdletBinding()]
param([string]$SignTool)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$signScript = Join-Path $PSScriptRoot 'sign.ps1'
$work       = Join-Path ([IO.Path]::GetTempPath()) "cryo-sign-tests-$([guid]::NewGuid().Guid)"
$password   = 'cryo-test-password'
$certVars   = @('CRYO_CERT_BASE64', 'CRYO_CERT_PASSWORD', 'CRYO_CERT_THUMBPRINT', 'CRYO_SIGNTOOL')

$script:passed     = 0
$script:failed     = 0
$script:lastTarget = $null

function New-TestCert([string]$subject, [hashtable]$extra) {
    $args = @{
        Subject           = $subject
        CertStoreLocation = 'Cert:\CurrentUser\My'
        KeyExportPolicy   = 'Exportable'
    } + $extra
    return New-SelfSignedCertificate @args
}

function Export-Base64([object]$cert, [string]$name) {
    $pfx = Join-Path $work "$name.pfx"
    Export-PfxCertificate -Cert $cert -FilePath $pfx `
        -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null
    return [Convert]::ToBase64String([IO.File]::ReadAllBytes($pfx))
}

# A fresh unsigned PE per case: signing rewrites the file, so cases must not
# inherit each other's signatures.
function New-TargetExe([string]$name) {
    $path = Join-Path $work "$name.exe"
    Add-Type -OutputType ConsoleApplication -OutputAssembly $path -TypeDefinition `
        'public class P { public static void Main() { System.Console.WriteLine("cryo test"); } }'
    return $path
}

function Invoke-Sign([hashtable]$vars, [string]$target) {
    foreach ($k in $certVars) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    if ($SignTool) { $env:CRYO_SIGNTOOL = $SignTool }
    foreach ($k in $vars.Keys) { Set-Item "Env:$k" $vars[$k] }

    $ErrorActionPreference = 'Continue'
    $lines = & powershell -NoProfile -ExecutionPolicy Bypass -File $signScript -Path $target 2>&1 |
        ForEach-Object {
            if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.ToString() } else { "$_" }
        }
    return [pscustomobject]@{
        Output   = ($lines -join [Environment]::NewLine)
        ExitCode = $LASTEXITCODE
    }
}

# Reports through the output stream and hands the signed file back through
# $script:lastTarget rather than returning it: a returned value would have to
# be discarded at every call site, and in PowerShell discarding a return value
# discards the log lines with it.
function Assert-Case([string]$name, [hashtable]$vars, [int]$expectExit, [string]$expectText) {
    $target = New-TargetExe ("t" + ($name -replace '[^a-zA-Z0-9]', ''))
    $script:lastTarget = $target
    $r = Invoke-Sign $vars $target

    $problems = @()
    if ($r.ExitCode -ne $expectExit) {
        $problems += "expected exit $expectExit, got $($r.ExitCode)"
    }
    if ($r.Output -notmatch [regex]::Escape($expectText)) {
        $problems += "output did not contain '$expectText'"
    }

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
$certs = @()

try {
    Write-Output "work dir: $work"

    $good = New-TestCert 'CN=cryo sign.ps1 test signer' @{
        Type     = 'CodeSigningCert'
        NotAfter = (Get-Date).AddYears(2)
    }
    $expired = New-TestCert 'CN=cryo sign.ps1 expired signer' @{
        Type      = 'CodeSigningCert'
        NotBefore = (Get-Date).AddDays(-400)
        NotAfter  = (Get-Date).AddDays(-30)
    }
    $tls = New-TestCert 'CN=cryo sign.ps1 tls cert' @{
        Type     = 'SSLServerAuthentication'
        NotAfter = (Get-Date).AddYears(2)
    }
    $certs = @($good, $expired, $tls)

    $goodB64    = Export-Base64 $good 'good'
    $expiredB64 = Export-Base64 $expired 'expired'
    $tlsB64     = Export-Base64 $tls 'tls'

    Write-Output ''
    Write-Output 'Configuration and preflight:'
    Assert-Case 'no certificate configured leaves the file unsigned' `
        @{} 0 'leaving'
    Assert-Case 'base64 without a password is refused' `
        @{ CRYO_CERT_BASE64 = $goodB64 } 1 'without CRYO_CERT_PASSWORD'
    Assert-Case 'two certificate sources at once are refused' `
        @{ CRYO_CERT_BASE64 = $goodB64; CRYO_CERT_PASSWORD = $password
           CRYO_CERT_THUMBPRINT = $good.Thumbprint } 1 'not both'
    Assert-Case 'armoured or mistyped base64 is named as such' `
        @{ CRYO_CERT_BASE64 = "-----BEGIN CERTIFICATE-----`nnope`n"
           CRYO_CERT_PASSWORD = $password } 1 'not valid base64'
    Assert-Case 'the wrong password is named as such' `
        @{ CRYO_CERT_BASE64 = $goodB64; CRYO_CERT_PASSWORD = 'wrong' } 1 'did not load as a PFX'
    Assert-Case 'an expired certificate is refused before signing' `
        @{ CRYO_CERT_BASE64 = $expiredB64; CRYO_CERT_PASSWORD = $password } 1 'expired'
    Assert-Case 'a non-code-signing certificate is refused' `
        @{ CRYO_CERT_BASE64 = $tlsB64; CRYO_CERT_PASSWORD = $password } 1 'not a code-signing'
    Assert-Case 'an absent thumbprint is named as such' `
        @{ CRYO_CERT_THUMBPRINT = '0000000000000000000000000000000000000000' } 1 'No certificate with thumbprint'
    Assert-Case 'a missing signtool is named as such' `
        @{ CRYO_CERT_BASE64 = $goodB64; CRYO_CERT_PASSWORD = $password
           CRYO_SIGNTOOL = 'C:\nope\signtool.exe' } 1 'does not exist'

    # The gate. signtool sign is happy to sign with a certificate Windows will
    # not honour, which is how an unusable signature reaches a release.
    Write-Output ''
    Write-Output 'Verification gate (certificate root not trusted yet):'
    Assert-Case 'a signature Windows would reject fails the build' `
        @{ CRYO_CERT_BASE64 = $goodB64; CRYO_CERT_PASSWORD = $password } 1 'Windows would reject'
    $gateTarget = $script:lastTarget

    # Reaching the gate at all means the store lookup found the certificate
    # and signtool signed with it, spaced-and-lowercased thumbprint included —
    # the form the Windows certificate dialog hands you to paste.
    $spaced = (($good.Thumbprint -split '(.{2})' | Where-Object { $_ }) -join ' ').ToLowerInvariant()
    Assert-Case 'a store or token certificate signs by thumbprint' `
        @{ CRYO_CERT_THUMBPRINT = $spaced } 1 'Windows would reject'

    # The gate rejected that file, but signing itself ran: what landed on disk
    # is a real Authenticode signature, and these read it back from the file
    # rather than trusting signtool's exit code.
    Write-Output ''
    Write-Output 'Signed artefact:'
    $sig = Get-AuthenticodeSignature $gateTarget
    $checks = @(
        @{ Name = 'the file carries an Authenticode signature'
           Ok   = ($sig.SignatureType -eq 'Authenticode'); Got = $sig.SignatureType }
        @{ Name = 'the signer is the test certificate'
           Ok   = ($sig.SignerCertificate.Thumbprint -eq $good.Thumbprint)
           Got  = $sig.SignerCertificate.Subject }
        @{ Name = 'an RFC 3161 authority countersigned it'
           Ok   = ($null -ne $sig.TimeStamperCertificate)
           Got  = $sig.TimeStamperCertificate.Subject }
        # Untrusted only because the root is self-signed. The signature is
        # well formed; this is the status the gate above acted on.
        @{ Name = 'it is untrusted for the expected reason'
           Ok   = ($sig.Status -eq 'UnknownError'); Got = $sig.Status }
        # The PFX is written to TEMP so signtool can read the key, and deleted
        # in a finally. A release that failed mid-signing must not leave one.
        @{ Name = 'no decoded PFX was left in TEMP'
           Ok   = (-not (Get-ChildItem ([IO.Path]::GetTempPath()) -Filter 'cryo-sign-*.pfx' -ErrorAction SilentlyContinue))
           Got  = 'temp is clean' }
    )

    # The gate's other half. Everything above proves it rejects a bad chain;
    # nothing proves `verify /pa` accepts a good one, and a wrong command form
    # there would fail every release the day a real certificate arrives. The
    # only trusted chain available without mutating a trust store is one
    # Microsoft already signed.
    $trustedBinary = if ($SignTool) { $SignTool } else {
        (Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
            Sort-Object FullName | Select-Object -Last 1).FullName
    }
    $ErrorActionPreference = 'Continue'
    & $trustedBinary verify /pa /all $trustedBinary 2>&1 | Out-Null
    $trustedExit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    $checks += @{ Name = 'verify /pa accepts a genuinely trusted signature'
                  Ok   = ($trustedExit -eq 0); Got = "exit $trustedExit" }

    foreach ($check in $checks) {
        if ($check.Ok) {
            Write-Output "  PASS  $($check.Name) -- $($check.Got)"
            $script:passed++
        } else {
            Write-Output "  FAIL  $($check.Name) -- got '$($check.Got)'"
            $script:failed++
        }
    }
} finally {
    foreach ($k in $certVars) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue

    # signtool can still hold the key container of the certificate it just
    # signed with, which makes the first delete fail. Retry, then say exactly
    # what is left rather than swallowing it — a test that cleans up silently
    # most of the time is a test that litters a developer's store.
    $stubborn = @()
    foreach ($c in $certs) {
        foreach ($attempt in 1..3) {
            Remove-Item "Cert:\CurrentUser\My\$($c.Thumbprint)" -Force -ErrorAction SilentlyContinue
            if (-not (Test-Path "Cert:\CurrentUser\My\$($c.Thumbprint)")) { break }
            Start-Sleep -Milliseconds 300
        }
        if (Test-Path "Cert:\CurrentUser\My\$($c.Thumbprint)") { $stubborn += $c.Thumbprint }
    }
    if ($stubborn) {
        Write-Warning ("Test certificates left in CurrentUser\My. Remove with:`n" +
            (($stubborn | ForEach-Object { "  Remove-Item Cert:\CurrentUser\My\$_ -Force" }) -join "`n"))
    }
}

Write-Output ''
Write-Output "$script:passed passed, $script:failed failed"
if ($script:failed -gt 0) { exit 1 }
