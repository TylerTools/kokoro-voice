# kokoro-voice installer — Windows.
#
# Idempotent: safe to re-run.
#
# STATUS: sets up the service and both command-line clients, which are complete
# and portable. The desktop host (global hotkeys, mini player, type-into-focused
# -app) is not built yet — see PORTING-WINDOWS.md.
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1

$ErrorActionPreference = 'Stop'

$Here       = Split-Path -Parent $MyInvocation.MyCommand.Path
$ConfigDir  = Join-Path $env:USERPROFILE '.config\kokoro'
$TokenFile  = Join-Path $ConfigDir 'token'
$Port       = if ($env:KOKORO_PORT) { $env:KOKORO_PORT } else { '8123' }
$ModelBase  = 'https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0'
$TaskName   = 'kokoro-voice'

function Say  ($m) { Write-Host "==> $m" -ForegroundColor Blue }
function Ok   ($m) { Write-Host "  $([char]0x2713) $m" -ForegroundColor Green }
function Warn ($m) { Write-Host "  ! $m" -ForegroundColor Yellow }
function Die  ($m) { Write-Host "  x $m" -ForegroundColor Red; exit 1 }

# ── 1. uv ────────────────────────────────────────────────────────────────────
Say 'Checking for uv'
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    $env:Path = "$env:USERPROFILE\.local\bin;$env:Path"
}
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Say 'Installing uv (manages Python without touching the system one)'
    Invoke-RestMethod https://astral.sh/uv/install.ps1 | Invoke-Expression
    $env:Path = "$env:USERPROFILE\.local\bin;$env:Path"
}
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Die 'uv install failed; see https://docs.astral.sh/uv/'
}
Ok "uv $((uv --version) -split ' ' | Select-Object -Last 1)"

# ── 2. environment ───────────────────────────────────────────────────────────
Say 'Creating the Python environment'
$Venv = Join-Path $Here '.venv'
if (-not (Test-Path $Venv)) { uv venv --python 3.12 $Venv }
$env:VIRTUAL_ENV = $Venv
uv pip install --quiet -r (Join-Path $Here 'requirements.txt')
uv pip install --quiet -r (Join-Path $Here 'requirements-windows.txt')

# The GPU decides the speech-to-text config, so report what we found rather
# than silently picking. See PORTING-WINDOWS.md section 3.
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    Ok 'NVIDIA GPU detected — use device="cuda", compute_type="float16"'
    Warn 'CUDA runtime + cuDNN must be installed for that path to work.'
} else {
    Warn 'No NVIDIA GPU — speech-to-text will run on the CPU.'
    Warn 'Benchmark int8 against float32: on ARM float32 won, but x86 with'
    Warn 'AVX-512/VNNI may invert that. Measure; do not assume.'
}
Ok 'Environment ready'

# ── 3. models ────────────────────────────────────────────────────────────────
# Not redistributed — fetched from upstream. Sizes are verified because a
# truncated download fails much later and far less obviously.
Say 'Downloading models (~500MB, first run only)'
$ModelDir = Join-Path $Here 'models'
New-Item -ItemType Directory -Force -Path $ModelDir | Out-Null
function Get-Model($Name, $Expected) {
    $Path = Join-Path $ModelDir $Name
    if (Test-Path $Path) {
        $Actual = (Get-Item $Path).Length
        if ($Actual -eq $Expected) { Ok "$Name already present"; return }
        Warn "$Name is $Actual bytes, expected $Expected - refetching"
    }
    Invoke-WebRequest -Uri "$ModelBase/$Name" -OutFile $Path
    $Actual = (Get-Item $Path).Length
    if ($Actual -ne $Expected) { Die "$Name downloaded $Actual bytes, expected $Expected" }
    Ok $Name
}
Get-Model 'kokoro-v1.0.fp16.onnx' 177464787
Get-Model 'voices-v1.0.bin'        28214398

# ── 4. auth token ────────────────────────────────────────────────────────────
Say 'Setting up the auth token'
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
if ((Test-Path $TokenFile) -and (Get-Item $TokenFile).Length -gt 0) {
    Ok 'Token already exists (left untouched)'
} else {
    $Py = Join-Path $Venv 'Scripts\python.exe'
    & $Py -c "import secrets; print(secrets.token_urlsafe(32))" | Set-Content -NoNewline $TokenFile
    Ok 'Generated a new token'
}
# Windows has no chmod; restrict the ACL to this user instead. Without this the
# token inherits directory permissions and may be readable by other accounts.
$Acl = Get-Acl $TokenFile
$Acl.SetAccessRuleProtection($true, $false)
$Acl.SetAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule(
    "$env:USERDOMAIN\$env:USERNAME", 'FullControl', 'Allow')))
Set-Acl $TokenFile $Acl
Ok 'Token restricted to this user'

# ── 5. service at login ──────────────────────────────────────────────────────
Say 'Registering the service to start at login'
New-Item -ItemType Directory -Force -Path (Join-Path $Here 'logs') | Out-Null
$Py = Join-Path $Venv 'Scripts\pythonw.exe'
if (-not (Test-Path $Py)) { $Py = Join-Path $Venv 'Scripts\python.exe' }
# Loopback only. Do NOT change to 0.0.0.0 — that publishes the service to every
# network this machine joins. To serve another machine, bind that ONE interface.
$Args = "-m uvicorn server:app --host 127.0.0.1 --port $Port"
$Action  = New-ScheduledTaskAction -Execute $Py -Argument $Args -WorkingDirectory $Here
$Trigger = New-ScheduledTaskTrigger -AtLogOn
$Settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -StartWhenAvailable -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger `
    -Settings $Settings -Description 'kokoro-voice local TTS/STT service' | Out-Null
[Environment]::SetEnvironmentVariable('HF_HUB_OFFLINE', '1', 'User')
Start-ScheduledTask -TaskName $TaskName
Ok 'Service registered and started'

# ── 6. verify ────────────────────────────────────────────────────────────────
# An installer that claims success without proving it is worse than none.
Say 'Verifying (the speech model takes a moment to warm up)'
$Health = $null
foreach ($i in 1..60) {
    try {
        $Health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 3
        break
    } catch { Start-Sleep -Seconds 2 }
}
if (-not $Health) { Die "Service did not come up. Check $Here\logs\" }
Write-Host "    $($Health | ConvertTo-Json -Compress)"
if ($Health.auth_required) { Ok 'Auth is on' } else { Warn "Auth is NOT on - check $TokenFile" }

$Token = (Get-Content $TokenFile -Raw).Trim()
try {
    Invoke-WebRequest -Uri "http://127.0.0.1:$Port/speak" -Method Post `
        -Headers @{ Authorization = "Bearer $Token" } -ContentType 'application/json' `
        -Body '{"text":"Installation complete."}' -TimeoutSec 30 -OutFile $null | Out-Null
    Ok 'Synthesis works'
} catch { Die "Synthesis failed: $_" }

Write-Host ''
Say 'Done.'
Write-Host "  Service:  http://127.0.0.1:$Port"
Write-Host "  Token:    $TokenFile"
Write-Host "  Logs:     $Here\logs\"
Write-Host ''
Write-Host "  Try it:   $Venv\Scripts\python.exe client\speak.py --text 'hello there'"
Write-Host ''
Warn 'Global hotkeys and the mini player are not built for Windows yet.'
Warn 'See PORTING-WINDOWS.md.'
