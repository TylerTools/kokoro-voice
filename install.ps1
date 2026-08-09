# Kokoro Voice Windows desktop installer bootstrap.
# Downloads only a signed release installer; the desktop app owns its engine.

$ErrorActionPreference = 'Stop'
$Repository = 'TylerTools/kokoro-voice'

Write-Host 'Finding the latest Kokoro Voice release...'
$release = Invoke-RestMethod "https://api.github.com/repos/$Repository/releases/latest"
$asset = $release.assets | Where-Object {
    $_.name -match 'x64.*\.(msi|exe)$' -or $_.name -match '\.(msi|exe)$'
} | Select-Object -First 1
if (-not $asset) { throw 'No Windows installer is attached to the latest release.' }
$checksumAsset = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
if (-not $checksumAsset) { throw 'The release is missing its signed-build checksum manifest.' }

$extension = [IO.Path]::GetExtension($asset.name)
$installer = Join-Path $env:TEMP "kokoro-voice-$($release.tag_name)$extension"
$checksumFile = Join-Path $env:TEMP "kokoro-voice-$($release.tag_name)-SHA256SUMS.txt"
Invoke-WebRequest $asset.browser_download_url -OutFile $installer
Invoke-WebRequest $checksumAsset.browser_download_url -OutFile $checksumFile

$escapedName = [Regex]::Escape($asset.name)
$checksumLine = Get-Content $checksumFile | Where-Object {
    $_ -match "^[0-9a-fA-F]{64}\s+$escapedName$"
} | Select-Object -First 1
if (-not $checksumLine) {
    Remove-Item $installer, $checksumFile -Force -ErrorAction SilentlyContinue
    throw "No published SHA-256 was found for $($asset.name)."
}
$expectedHash = ($checksumLine -split '\s+')[0].ToLowerInvariant()
$actualHash = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) {
    Remove-Item $installer, $checksumFile -Force -ErrorAction SilentlyContinue
    throw 'Installer SHA-256 does not match the published release manifest.'
}
Remove-Item $checksumFile -Force -ErrorAction SilentlyContinue

$signature = Get-AuthenticodeSignature $installer
if ($signature.Status -ne 'Valid') {
    Remove-Item $installer -Force -ErrorAction SilentlyContinue
    throw "Installer signature is not valid: $($signature.Status)"
}

Write-Host "Verified publisher: $($signature.SignerCertificate.Subject)"
if ($extension -eq '.msi') {
    $process = Start-Process msiexec.exe -ArgumentList @('/i', $installer) -Wait -PassThru
} else {
    $process = Start-Process $installer -Wait -PassThru
}
if ($process.ExitCode -ne 0) { throw "Installer exited with code $($process.ExitCode)." }
Write-Host 'Kokoro Voice installed. First launch will download and verify its local models.'
