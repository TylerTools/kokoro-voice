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

$extension = [IO.Path]::GetExtension($asset.name)
$installer = Join-Path $env:TEMP "kokoro-voice-$($release.tag_name)$extension"
Invoke-WebRequest $asset.browser_download_url -OutFile $installer

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
