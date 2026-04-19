<#
.SYNOPSIS
    nodex uninstaller for Windows.

.PARAMETER InstallDir
    Binary directory (default: $env:USERPROFILE\.local\bin).

.PARAMETER KeepSkill
    Do not remove user-level skill.

.PARAMETER KeepBackup
    Do not back up skill before removal.

.PARAMETER Yes
    Non-interactive mode.

.EXAMPLE
    .\scripts\uninstall.ps1
#>

[CmdletBinding()]
param(
    [string]$InstallDir = $(if ($env:NODEX_INSTALL_DIR) { $env:NODEX_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".local\bin" }),
    [switch]$KeepSkill  = ($env:NODEX_KEEP_SKILL  -eq "1"),
    [switch]$KeepBackup = ($env:NODEX_KEEP_BACKUP -eq "1"),
    [switch]$Yes        = ($env:NODEX_YES         -eq "1")
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Script:BinaryName = "nodex"
$Script:SkillName  = "nodex"

function Write-Step { param([string]$Message) Write-Host "▸  $Message" -ForegroundColor Blue }
function Write-Ok   { param([string]$Message) Write-Host "✓  $Message" -ForegroundColor Green }
function Write-Warn { param([string]$Message) Write-Host "!  $Message" -ForegroundColor Yellow }
function Write-Info { param([string]$Message) Write-Host "   $Message" -ForegroundColor DarkGray }

function Test-Interactive {
    if ($Yes) { return $false }
    try { return [Environment]::UserInteractive -and -not [Console]::IsInputRedirected } catch { return $true }
}

function Read-YesNo {
    param([string]$Question, [bool]$DefaultYes = $false)
    if (-not (Test-Interactive)) { return $DefaultYes }
    $default = if ($DefaultYes) { 0 } else { 1 }
    $choices = @(
        [System.Management.Automation.Host.ChoiceDescription]::new("&Yes", "Yes"),
        [System.Management.Automation.Host.ChoiceDescription]::new("&No", "No")
    )
    $idx = $Host.UI.PromptForChoice($Question, $null, [System.Management.Automation.Host.ChoiceDescription[]]$choices, $default)
    return ($idx -eq 0)
}

function Backup-Path {
    param([string]$Target)
    if (-not (Test-Path $Target)) { return }
    $stamp = Get-Date -Format "yyyyMMdd_HHmmss"
    $backup = "$Target.backup_$stamp"
    Copy-Item -Path $Target -Destination $backup -Recurse -Force
    Write-Info "Backup: $backup"
}

function Uninstall-Binary {
    $dest = Join-Path $InstallDir "$Script:BinaryName.exe"
    Write-Step "Removing binary"
    if (Test-Path $dest) {
        Remove-Item -Path $dest -Force
        Write-Ok "Removed $dest"
    } else {
        Write-Info "Binary not found at $dest"
    }
}

function Uninstall-Skill {
    $target = Join-Path $env:USERPROFILE ".claude\skills\$Script:SkillName"
    if (-not (Test-Path $target)) { Write-Info "No user-level skill at $target"; return }
    if ($KeepSkill) { Write-Info "Keeping skill (-KeepSkill)"; return }
    # -Yes means non-interactive full cleanup. Only interactive runs prompt
    # (default No) since skills can outlive the binary across projects.
    if (-not $Yes -and -not (Read-YesNo "Remove skill at $target?" $false)) {
        Write-Info "Skill kept"; return
    }

    Write-Step "Removing skill"
    if (-not $KeepBackup) { Backup-Path $target }
    Remove-Item -Path $target -Recurse -Force
    Write-Ok "Removed $target"

    $parent = Join-Path $env:USERPROFILE ".claude\skills"
    if ((Test-Path $parent) -and -not (Get-ChildItem $parent -Force)) {
        Remove-Item -Path $parent -Force
        Write-Info "Cleaned empty $parent"
    }
}

function Start-Uninstall {
    Write-Host ""
    Write-Host "nodex uninstaller" -ForegroundColor White
    Write-Host ""
    Uninstall-Binary
    Uninstall-Skill
    Write-Host ""
    Write-Host "✅ Uninstall complete" -ForegroundColor Green
    Write-Info "Project-level skills (.claude\skills\$Script:SkillName\) are managed by git"
}

Start-Uninstall
