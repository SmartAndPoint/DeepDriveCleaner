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
# The firewall rule this script adds (and, often, the one the capability created) covers
# the Private network profile: a network marked Public refuses ssh until it is marked Private.
#
# On a lab machine it also makes the Recycle Bin test volume (story "Windows scanner and
# rule catalog", step 10b), alone or together with the steps above:
#
#   .\windows-host-setup.ps1 -TestVolume [-TestVolumeLetter R]
#   .\windows-host-setup.ps1 -RemoveTestVolume
#
# That is a 1 GB expandable VHDX in the work folder with one NTFS partition and a drive
# letter (the first free one from R, or -TestVolumeLetter), attached again at each startup
# by a scheduled task. The tests that set a small Recycle Bin size run on it through
# DDC_RECYCLE_TEST_VOLUME, so the system drive's bin keeps its size. -RemoveTestVolume
# detaches it and deletes the file and the task; the volume's Recycle Bin key under HKCU
# stays, harmless.

param(
    [string]$PubKey,
    [string]$Dir = 'ddc-native',
    [switch]$TestVolume,
    [switch]$RemoveTestVolume,
    [string]$TestVolumeLetter
)

$ErrorActionPreference = 'Stop'
$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this in PowerShell opened as Administrator.'
}
function Step($m) { Write-Host "`n== $m" -ForegroundColor Cyan }
if (-not ($PubKey -or $TestVolume -or $RemoveTestVolume)) {
    throw 'Pass -PubKey to prepare the machine, -TestVolume or -RemoveTestVolume for the test volume.'
}
$work = Join-Path $env:USERPROFILE $Dir
$vhdx = Join-Path $work 'recycle-test.vhdx'
$task = 'ddc recycle test volume'

if ($RemoveTestVolume) {
    Step 'removing the Recycle Bin test volume'
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    if (Test-Path $vhdx) {
        if ((Get-DiskImage -ImagePath $vhdx).Attached) { Dismount-DiskImage -ImagePath $vhdx | Out-Null }
        Remove-Item $vhdx
    }
}

if ($TestVolume) {
    Step 'the Recycle Bin test volume'
    New-Item -ItemType Directory -Force $work | Out-Null
    if (-not (Test-Path $vhdx)) {
        # diskpart, not New-VHD: that one needs the Hyper-V module, which Home editions lack.
        $commands = Join-Path $env:TEMP 'ddc-recycle-test-vhdx.txt'
        "create vdisk file=`"$vhdx`" maximum=1024 type=expandable" | Set-Content -Encoding ascii $commands
        diskpart /s $commands | Out-Null
        $code = $LASTEXITCODE
        Remove-Item $commands
        if ($code -ne 0) { throw "diskpart could not create $vhdx" }
    }
    if (-not (Get-DiskImage -ImagePath $vhdx).Attached) { Mount-DiskImage -ImagePath $vhdx | Out-Null }
    $disk = Get-DiskImage -ImagePath $vhdx | Get-Disk
    if ($disk.IsOffline) { Set-Disk -Number $disk.Number -IsOffline $false }
    if ($disk.PartitionStyle -eq 'RAW') { Initialize-Disk -Number $disk.Number -PartitionStyle GPT }
    $part = Get-Partition -DiskNumber $disk.Number | Where-Object Type -eq 'Basic' | Select-Object -First 1
    # Formatted before it gets a letter, so Explorer offers no "format this disk" dialog;
    # a partition an earlier run left unformatted is formatted now.
    if (-not $part) { $part = New-Partition -DiskNumber $disk.Number -UseMaximumSize }
    if ((Get-Volume -Partition $part).FileSystem -ne 'NTFS') {
        Format-Volume -Partition $part -FileSystem NTFS -NewFileSystemLabel 'ddc-recycle-test' -Confirm:$false | Out-Null
    }
    $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
    if ($TestVolumeLetter -and "$($part.DriveLetter)" -match '^[A-Z]$' -and
        "$($part.DriveLetter)" -ne $TestVolumeLetter.Substring(0, 1).ToUpper()) {
        Write-Warning "the test volume already has the letter $($part.DriveLetter); -TestVolumeLetter is not applied"
    }
    if ("$($part.DriveLetter)" -notmatch '^[A-Z]$') {
        $used = [IO.DriveInfo]::GetDrives() | ForEach-Object { $_.Name.Substring(0, 1).ToUpper() }
        if ($TestVolumeLetter) {
            $letter = $TestVolumeLetter.Substring(0, 1).ToUpper()
            if ($used -contains $letter) { throw "drive letter $letter is taken; pass another -TestVolumeLetter" }
        } else {
            $letter = 'RSTUVWXYZ'.ToCharArray() | ForEach-Object { "$_" } | Where-Object { $used -notcontains $_ } |
                Select-Object -First 1
            if (-not $letter) { throw 'no free drive letter from R to Z; pass -TestVolumeLetter' }
        }
        Set-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber -NewDriveLetter $letter
        $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $part.PartitionNumber
    }
    if (-not (Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue)) {
        $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
            -Argument "-NoProfile -NonInteractive -Command Mount-DiskImage -ImagePath '$($vhdx.Replace("'", "''"))'"
        Register-ScheduledTask -TaskName $task -Action $action -Trigger (New-ScheduledTaskTrigger -AtStartup) `
            -User 'NT AUTHORITY\SYSTEM' -RunLevel Highest | Out-Null
    }
    $root = "$($part.DriveLetter):\"
    Write-Host "The test volume is $root. On the developer machine, for the tests that need it:"
    Write-Host "  DDC_RECYCLE_TEST_VOLUME='$root' DDC_WINDOWS_HOST=<that host> scripts/native-windows.sh -E 'binary(recycle_bin)'"
}

if (-not $PubKey) { Step 'done'; exit 0 }

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
New-Item -ItemType Directory -Force $work | Out-Null
Add-MpPreference -ExclusionPath $work

Restart-Service sshd
Step 'done'
Get-NetConnectionProfile | Format-Table InterfaceAlias, NetworkCategory
Write-Host "Add an ssh host for this machine on the developer machine, then run:"
Write-Host "  DDC_WINDOWS_HOST=<that host> scripts/native-windows.sh --setup"
