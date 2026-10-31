# Prepares a Windows machine for scripts/native-windows.sh, the Gate's Windows leg over
# ssh (story "Windows scanner and rule catalog", DDC-007). Its owner runs it once, in
# PowerShell opened as Administrator:
#
#   Set-ExecutionPolicy -Scope Process Bypass -Force
#   .\windows-host-setup.ps1 -PubKey '<the developer machine's ssh public key>'
#
# It installs what needs an administrator: OpenSSH Server (with Git bash as the ssh
# shell and port 22 open), Git for Windows and Visual Studio Build Tools with the C++
# workload. It turns on long paths and makes one work folder in the home, excluded from
# Defender's scan. Rust is not installed here: `native-windows.sh --setup` puts rustup,
# the toolchain and nextest inside that folder, off the PATH. Running it again installs
# nothing twice; it restarts sshd at the end, which drops open ssh sessions.
#
# The firewall rule OpenSSH adds covers the Private network profile: a network marked
# Public refuses ssh until it is marked Private.

param(
    [Parameter(Mandatory = $true)][string]$PubKey,
    [string]$Dir = 'ddc-native'
)

$ErrorActionPreference = 'Stop'
$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in PowerShell opened as Administrator.'
}
function Step($m) { Write-Host "`n== $m" -ForegroundColor Cyan }

Step 'OpenSSH Server'
$cap = Get-WindowsCapability -Online -Name 'OpenSSH.Server*'
if ($cap.State -ne 'Installed') { Add-WindowsCapability -Online -Name $cap.Name | Out-Null }
Set-Service sshd -StartupType Automatic
Start-Service sshd
if (-not (Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' `
        -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 -Profile Private | Out-Null
}

# sshd reads an administrator's keys from ProgramData, and only when Administrators and
# SYSTEM alone can touch that file; anyone else's from ~\.ssh. The key goes in both.
Step 'authorized key'
New-Item -ItemType Directory -Force (Join-Path $env:USERPROFILE '.ssh') | Out-Null
$adminKeys = Join-Path $env:ProgramData 'ssh\administrators_authorized_keys'
foreach ($keys in @((Join-Path $env:USERPROFILE '.ssh\authorized_keys'), $adminKeys)) {
    if (-not (Test-Path $keys) -or -not (Select-String -Path $keys -SimpleMatch $PubKey.Split(' ')[1] -Quiet)) {
        Add-Content -Path $keys -Value $PubKey -Encoding ascii
    }
}
icacls $adminKeys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F' | Out-Null

Step 'Git for Windows, Visual Studio Build Tools (C++)'
function WingetInstall($id, $extra) {
    winget list --id $id -e --accept-source-agreements | Out-Null
    if ($LASTEXITCODE -eq 0) { Write-Host "$id is already installed"; return }
    winget install --id $id -e --accept-source-agreements --accept-package-agreements @extra
    if ($LASTEXITCODE -ne 0) { throw "winget could not install $id" }
}
WingetInstall 'Git.Git' @()
WingetInstall 'Microsoft.VisualStudio.2022.BuildTools' @('--override',
    '--wait --quiet --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')

Step 'ssh shell: Git bash'
$bash = 'C:\Program Files\Git\bin\bash.exe'
if (-not (Test-Path $bash)) { throw "Git bash not found at $bash" }
New-Item -Path 'HKLM:\SOFTWARE\OpenSSH' -Force | Out-Null
New-ItemProperty -Path 'HKLM:\SOFTWARE\OpenSSH' -Name DefaultShell -Value $bash -PropertyType String -Force | Out-Null

Step 'long paths, the work folder, its Defender exclusion'
New-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled `
    -Value 1 -PropertyType DWord -Force | Out-Null
& 'C:\Program Files\Git\cmd\git.exe' config --system core.longpaths true
$work = Join-Path $env:USERPROFILE $Dir
New-Item -ItemType Directory -Force $work | Out-Null
Add-MpPreference -ExclusionPath $work

Restart-Service sshd
Step 'done'
Get-NetConnectionProfile | Format-Table InterfaceAlias, NetworkCategory
Write-Host "Add an ssh host for this machine on the developer machine, then run:"
Write-Host "  DDC_WINDOWS_HOST=<that host> scripts/native-windows.sh --setup"
