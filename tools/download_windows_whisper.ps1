# Download the repository-pinned Windows speech model into its standard HF cache.
# This helper does not run an engine. A model is usable only after SHA-256 matches.
$ErrorActionPreference = 'Stop'
$revision = '0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf'
$expectedHash = 'e76620f83d5f5b69efd3d87e3dc180c1bd21df9fbebacfd4335e5e1efcc018da'
$snapshot = Join-Path $env:USERPROFILE ".cache\huggingface\hub\models--dropbox-dash--faster-whisper-large-v3-turbo\snapshots\$revision"
$destination = Join-Path $snapshot 'model.bin'
$partial = Join-Path $snapshot 'model.bin.local-download'
New-Item -ItemType Directory -Path $snapshot -Force | Out-Null
if (Test-Path -LiteralPath $destination) {
    if ((Get-FileHash -LiteralPath $destination).Hash.ToLower() -eq $expectedHash) {
        Write-Output 'Pinned Whisper model already verified.'
        exit 0
    }
    throw 'Existing model does not match the pinned checksum.'
}
$http = [System.Net.Http.HttpClient]::new()
$http.Timeout = [TimeSpan]::FromMinutes(30)
$response = $http.GetAsync("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/$revision/model.bin", [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
$response.EnsureSuccessStatusCode() | Out-Null
$total = $response.Content.Headers.ContentLength
$inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
$outputStream = [IO.File]::Create($partial)
$buffer = [byte[]]::new(4MB)
$received = 0L
$nextReport = 0L
try {
    while (($count = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
        $outputStream.Write($buffer, 0, $count)
        $received += $count
        if ($received -ge $nextReport) {
            Write-Output ('Whisper model: {0:N0} / {1:N0} MB' -f ($received / 1MB), ($total / 1MB))
            $nextReport = $received + 100MB
        }
    }
} finally {
    $outputStream.Dispose()
    $inputStream.Dispose()
    $response.Dispose()
    $http.Dispose()
}
if ((Get-FileHash -LiteralPath $partial).Hash.ToLower() -ne $expectedHash) {
    throw 'Whisper model SHA-256 mismatch; partial file was not installed.'
}
Move-Item -LiteralPath $partial -Destination $destination
Write-Output 'Pinned Whisper model downloaded and SHA-256 verified.'
