# Release-only Windows signing boundary. Tauri passes each executable path to
# this script; it fails closed unless Azure Artifact Signing and the expected
# publisher identity are configured in the protected release environment.
param(
    [Parameter(Mandatory = $true)]
    [string]$FilePath
)

$ErrorActionPreference = 'Stop'
$required = @(
    'AZURE_CLIENT_ID',
    'AZURE_CLIENT_SECRET',
    'AZURE_TENANT_ID',
    'WINDOWS_SIGNING_ENDPOINT',
    'WINDOWS_SIGNING_ACCOUNT',
    'WINDOWS_SIGNING_PROFILE',
    'WINDOWS_EXPECTED_PUBLISHER'
)

foreach ($name in $required) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Missing required release signing setting: $name"
    }
}

if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
    throw "Signing target does not exist: $FilePath"
}

& artifact-signing-cli `
    -e $env:WINDOWS_SIGNING_ENDPOINT `
    -a $env:WINDOWS_SIGNING_ACCOUNT `
    -c $env:WINDOWS_SIGNING_PROFILE `
    -d 'HereWord' `
    $FilePath
if ($LASTEXITCODE -ne 0) {
    throw "Artifact Signing failed with exit code $LASTEXITCODE"
}

$signature = Get-AuthenticodeSignature -LiteralPath $FilePath
if ($signature.Status -ne 'Valid') {
    throw "Authenticode verification failed: $($signature.Status)"
}
if ($signature.SignerCertificate.Subject -notlike "*$env:WINDOWS_EXPECTED_PUBLISHER*") {
    throw "Unexpected Windows publisher: $($signature.SignerCertificate.Subject)"
}

