[CmdletBinding(DefaultParameterSetName='WiFi')]
param(
    [Parameter(Mandatory,ParameterSetName='WiFi')][ipaddress]$PhoneAddress,
    [Parameter(Mandatory,ParameterSetName='Usb')][switch]$Usb,
    [Parameter(Mandatory,ParameterSetName='Remove')][switch]$Uninstall,
    [ValidatePattern('^[a-fA-F0-9-]+$')][string]$Device,
    [string]$Binary = (Join-Path $PSScriptRoot 'target\release\icloud-copy-board.exe')
)
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$name = "ClipboardNative-$($identity.User.Value)"
$dir = Join-Path $env:LOCALAPPDATA 'ClipboardNative'
$destination = Join-Path $dir 'icloud-copy-board.exe'
$previous = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue

if (-not $Uninstall) { $source = (Resolve-Path -LiteralPath $Binary).Path }
if ($previous) {
    $running = @(Get-Process -Name 'icloud-copy-board' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $destination -and $_.SessionId -eq [Diagnostics.Process]::GetCurrentProcess().SessionId })
    Stop-ScheduledTask -TaskName $name
    foreach ($process in $running) {
        if (-not $process.WaitForExit(15000)) { throw 'The previous clipboard task is still running.' }
    }
    for ($i = 0; $i -lt 50 -and (Get-ScheduledTask -TaskName $name).State -eq 'Running'; $i++) {
        Start-Sleep -Milliseconds 100
    }
}
if ($Uninstall) {
    if ($previous) { Unregister-ScheduledTask -TaskName $name -Confirm:$false }
    if (Test-Path -LiteralPath $destination) { Remove-Item -LiteralPath $destination }
    return
}

New-Item -ItemType Directory -Path $dir -Force | Out-Null
if ($source -ne $destination) {
    $staged = Join-Path $dir ([guid]::NewGuid().ToString() + '.tmp')
    try {
        Copy-Item -LiteralPath $source -Destination $staged
        if (Test-Path -LiteralPath $destination) {
            for ($i = 0; ; $i++) {
                try { [IO.File]::Replace($staged, $destination, [System.Management.Automation.Language.NullString]::Value); break }
                catch [IO.IOException] {
                    if ($i -ge 20) { throw }
                    Start-Sleep -Milliseconds 250
                }
            }
        } else { [IO.File]::Move($staged, $destination) }
    } catch {
        if ($previous) { Start-ScheduledTask -TaskName $name }
        throw
    } finally {
        if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged }
    }
}
$arguments = if ($Usb) { '--usb' } else { "--host $($PhoneAddress.IPAddressToString)" }
if ($Device) { $arguments += " --device $Device" }
$action = New-ScheduledTaskAction -Execute $destination -Argument $arguments
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity.Name
$principal = New-ScheduledTaskPrincipal -UserId $identity.Name -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([timespan]::Zero) -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null
Start-ScheduledTask -TaskName $name
Write-Output "Installed user task $name. Status: $dir\status.txt"
