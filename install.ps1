# HereWord Windows desktop installer bootstrap.
# Downloads one signed NSIS artifact from the approved binary-only release
# repository. It never accepts or stores a source-repository credential.
param(
    [string]$ReleaseRepository = $env:HEREWORD_RELEASE_REPOSITORY,
    [string]$ReleaseTag = ''
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($ReleaseRepository)) {
    throw 'Windows distribution is not active. Set HEREWORD_RELEASE_REPOSITORY to the approved public binary release repository.'
}
if ($ReleaseRepository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    throw 'ReleaseRepository must use the GitHub owner/repository form.'
}

$releasePath = if ([string]::IsNullOrWhiteSpace($ReleaseTag)) {
    'latest'
} else {
    "tags/$ReleaseTag"
}
$release = Invoke-RestMethod "https://api.github.com/repos/$ReleaseRepository/releases/$releasePath"
$assets = @($release.assets | Where-Object {
    $_.name -match '^HereWord_.*_x64-setup\.exe$'
})
if ($assets.Count -ne 1) {
    throw "Expected exactly one HereWord x64 NSIS installer; found $($assets.Count)."
}

$installer = Join-Path $env:TEMP $assets[0].name
Invoke-WebRequest $assets[0].browser_download_url -OutFile $installer

$signature = Get-AuthenticodeSignature -LiteralPath $installer
if ($signature.Status -ne 'Valid') {
    Remove-Item $installer -Force -ErrorAction SilentlyContinue
    throw "Installer signature is not valid: $($signature.Status)"
}
$expectedPublisher = $env:HEREWORD_EXPECTED_PUBLISHER
if (-not [string]::IsNullOrWhiteSpace($expectedPublisher) -and
    $signature.SignerCertificate.Subject -notlike "*$expectedPublisher*") {
    Remove-Item $installer -Force -ErrorAction SilentlyContinue
    throw "Installer publisher is not approved: $($signature.SignerCertificate.Subject)"
}

Write-Host "Verified publisher: $($signature.SignerCertificate.Subject)"
$process = Start-Process $installer -Wait -PassThru
if ($process.ExitCode -ne 0) {
    throw "Installer exited with code $($process.ExitCode)."
}
Write-Host 'HereWord installed. First launch will download and verify its local models.'
