<#
.SYNOPSIS
    nodex installer for Windows.

.DESCRIPTION
    Downloads a prebuilt nodex binary (or builds from source), verifies SHA256,
    installs to $env:USERPROFILE\.local\bin, and optionally installs the
    Claude Code skill. Supports both direct execution and `iwr | iex` piping.

.PARAMETER Version
    Install a specific version (default: latest release).

.PARAMETER InstallDir
    Binary install directory (default: $env:USERPROFILE\.local\bin).

.PARAMETER Skill
    Skill install level: user | project | none (default: user).

.PARAMETER FromSource
    Build from source using cargo instead of downloading prebuilt.

.PARAMETER Force
    Overwrite existing install without prompting.

.PARAMETER Yes
    Accept all defaults non-interactively.

.PARAMETER DryRun
    Print plan, do not execute.

.EXAMPLE
    iwr -useb https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.ps1 | iex

.EXAMPLE
    .\scripts\install.ps1 -Skill project -Yes
#>

[CmdletBinding()]
param(
    [string]$Version    = $env:NODEX_VERSION,
    [string]$InstallDir = $(if ($env:NODEX_INSTALL_DIR) { $env:NODEX_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".local\bin" }),
    [ValidateSet("user", "project", "none", "")]
    [string]$Skill      = $env:NODEX_SKILL_LEVEL,
    [switch]$FromSource = ($env:NODEX_FROM_SOURCE -eq "1"),
    [switch]$Force      = ($env:NODEX_FORCE -eq "1"),
    [switch]$Yes        = ($env:NODEX_YES -eq "1"),
    [switch]$DryRun     = ($env:NODEX_DRY_RUN -eq "1")
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Script:Repo        = "junyeong-ai/nodex"
$Script:BinaryName  = "nodex"
$Script:SkillName   = "nodex"
$Script:ApiBase     = "https://api.github.com/repos/$Script:Repo"
$Script:ReleaseBase = "https://github.com/$Script:Repo/releases/download"
$Script:TmpDir      = $null

# Track whether the user explicitly overrode each setting (via flag OR env).
# Prompts are skipped for any setting with an Explicit* flag set.
$Script:ExplicitInstallDir = $PSBoundParameters.ContainsKey('InstallDir') -or -not [string]::IsNullOrEmpty($env:NODEX_INSTALL_DIR)
$Script:ExplicitVersion    = $PSBoundParameters.ContainsKey('Version')    -or -not [string]::IsNullOrEmpty($env:NODEX_VERSION)
$Script:ExplicitSkill      = $PSBoundParameters.ContainsKey('Skill')      -or -not [string]::IsNullOrEmpty($env:NODEX_SKILL_LEVEL)
$Script:ExplicitFromSource = $PSBoundParameters.ContainsKey('FromSource') -or ($env:NODEX_FROM_SOURCE -eq "1")

# ═════════════════════════════ LOG ═════════════════════════════════════════

function Write-Step { param([string]$Message) Write-Host "▸  $Message" -ForegroundColor Blue }
function Write-Ok   { param([string]$Message) Write-Host "✓  $Message" -ForegroundColor Green }
function Write-Warn { param([string]$Message) Write-Host "!  $Message" -ForegroundColor Yellow }
function Write-Info { param([string]$Message) Write-Host "   $Message" -ForegroundColor DarkGray }
function Stop-Installer { param([string]$Message) Write-Host "✗  $Message" -ForegroundColor Red; exit 1 }

# ═════════════════════════════ PROMPTS ═════════════════════════════════════

function Test-Interactive {
    if ($Yes) { return $false }
    try { return [Environment]::UserInteractive -and -not [Console]::IsInputRedirected } catch { return $true }
}

function Read-Choice {
    param([string]$Title, [string[]]$Options, [int]$DefaultIndex = 0)
    if (-not (Test-Interactive)) { return $DefaultIndex }
    $choices = $Options | ForEach-Object {
        [System.Management.Automation.Host.ChoiceDescription]::new("&$_", $_)
    }
    $collection = [System.Management.Automation.Host.ChoiceDescription[]]$choices
    return $Host.UI.PromptForChoice($Title, $null, $collection, $DefaultIndex)
}

function Read-YesNo {
    param([string]$Question, [bool]$DefaultYes = $true)
    if (-not (Test-Interactive)) { return $DefaultYes }
    $default = if ($DefaultYes) { 0 } else { 1 }
    $idx = Read-Choice -Title $Question -Options @("Yes", "No") -DefaultIndex $default
    return ($idx -eq 0)
}

function Read-Path {
    param([string]$Question, [string]$Default)
    if (-not (Test-Interactive)) { return $Default }
    $answer = Read-Host -Prompt "$Question [$Default]"
    if ([string]::IsNullOrWhiteSpace($answer)) { return $Default }
    return $answer
}

# ═════════════════════════════ DETECT ══════════════════════════════════════

function Get-Platform {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        "AMD64" { return "x86_64-pc-windows-msvc" }
        "ARM64" { Stop-Installer "ARM64 Windows is not yet supported. Use -FromSource." }
        default { Stop-Installer "Unsupported architecture: $arch" }
    }
}

function Get-LatestVersion {
    $response = Invoke-RestMethod -Uri "$Script:ApiBase/releases/latest" -UseBasicParsing
    return $response.tag_name.TrimStart('v')
}

function Resolve-Version {
    if ($Version) { return $Version }
    try { return Get-LatestVersion }
    catch { Stop-Installer "Cannot fetch latest version: $_" }
}

# ═════════════════════════════ DOWNLOAD ════════════════════════════════════

function Invoke-ReleaseDownload {
    param([string]$TargetVersion, [string]$Platform, [string]$ArchiveName)
    $url = "$Script:ReleaseBase/v$TargetVersion/$ArchiveName"
    Write-Step "Downloading $ArchiveName"
    try {
        Invoke-WebRequest -Uri $url -OutFile (Join-Path $Script:TmpDir $ArchiveName) -UseBasicParsing
        Invoke-WebRequest -Uri "$url.sha256" -OutFile (Join-Path $Script:TmpDir "$ArchiveName.sha256") -UseBasicParsing
    } catch {
        Stop-Installer "Download failed: $url — $_"
    }
    Write-Ok "Downloaded"
}

function Test-Checksum {
    param([string]$ArchiveName)
    Write-Step "Verifying SHA256"
    $archive  = Join-Path $Script:TmpDir $ArchiveName
    $sumFile  = "$archive.sha256"
    $expected = (Get-Content $sumFile -Raw).Trim().Split()[0].ToLower()
    $actual   = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        Stop-Installer "Checksum mismatch: expected $expected, got $actual"
    }
    Write-Ok "Checksum match"
}

function Expand-ReleaseArchive {
    param([string]$ArchiveName)
    Write-Step "Extracting"
    $archive = Join-Path $Script:TmpDir $ArchiveName
    Expand-Archive -Path $archive -DestinationPath $Script:TmpDir -Force
    Write-Ok "Extracted"
}

function Test-Writable {
    param([string]$Dir)
    if (Test-Path $Dir) {
        try { $probe = Join-Path $Dir ".nodex-write-probe"; [IO.File]::WriteAllText($probe, ""); Remove-Item $probe -Force; return $true }
        catch { return $false }
    }
    $parent = Split-Path $Dir
    if (-not (Test-Path $parent)) { return $false }
    try { $probe = Join-Path $parent ".nodex-write-probe"; [IO.File]::WriteAllText($probe, ""); Remove-Item $probe -Force; return $true }
    catch { return $false }
}

function Install-Binary {
    param([string]$SourcePath, [string]$DestDir)
    $dest = Join-Path $DestDir "$Script:BinaryName.exe"
    if (-not (Test-Writable $DestDir)) {
        Stop-Installer "Install dir not writable: $DestDir`n  Try: -InstallDir `"$env:USERPROFILE\.local\bin`"`n  Or run PowerShell as Administrator for system paths."
    }
    Write-Step "Installing binary to $dest"
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
    Copy-Item -Path $SourcePath -Destination $dest -Force
    Write-Ok $dest
}

function Build-FromSource {
    param([string]$RepoDir)
    Write-Step "Building from source (cargo build --release)"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Stop-Installer "cargo not found — install Rust from https://rustup.rs"
    }
    Push-Location $RepoDir
    try { & cargo build --release --quiet --package nodex-cli; if ($LASTEXITCODE -ne 0) { Stop-Installer "cargo build failed" } }
    finally { Pop-Location }
    return (Join-Path $RepoDir "target\release\$Script:BinaryName.exe")
}

# ═════════════════════════════ SKILL ═══════════════════════════════════════

function Get-SkillVersion {
    param([string]$SkillMd)
    if (-not (Test-Path $SkillMd)) { return "" }
    $line = Select-String -Path $SkillMd -Pattern '^version:' -SimpleMatch:$false | Select-Object -First 1
    if (-not $line) { return "" }
    return ($line.Line -replace '^version:\s*', '').Trim()
}

function Compare-SemVer {
    param([string]$A, [string]$B)
    if ([string]::IsNullOrEmpty($A) -or [string]::IsNullOrEmpty($B)) { return "unknown" }
    # Strip a leading "v" so "v1.2.3" and "1.2.3" compare as equal.
    $aStr = $A -replace '^v', ''
    $bStr = $B -replace '^v', ''
    if ($aStr -eq $bStr) { return "equal" }
    # [version] cannot represent prerelease/build metadata (1.2.3-rc.1).
    # If either side carries non-numeric characters after the leading "v"
    # strip, fall back to "unknown" rather than silently mis-ordering them.
    $numericOnly = '^\d+(\.\d+){0,3}$'
    if ($aStr -notmatch $numericOnly -or $bStr -notmatch $numericOnly) { return "unknown" }
    try {
        $va = [version]$aStr
        $vb = [version]$bStr
        if ($va -lt $vb) { return "older" } else { return "newer" }
    } catch { return "unknown" }
}

function Backup-Path {
    param([string]$Target)
    if (-not (Test-Path $Target)) { return }
    # Include PID so two runs in the same second never collide.
    $stamp = Get-Date -Format "yyyyMMdd_HHmmss"
    $backup = "$Target.backup_${stamp}_$PID"
    Copy-Item -Path $Target -Destination $backup -Recurse -Force
    Write-Info "Backup: $backup"
}

# Download, verify, and extract the skill release asset. Returns the
# extracted skill directory path on success, or $null on any failure
# (caller treats $null as "skip the skill install").
function Get-SkillTarball {
    param([string]$TargetVersion)
    $archive = "$Script:BinaryName-skill-v$TargetVersion.tar.gz"
    $url     = "$Script:ReleaseBase/v$TargetVersion/$archive"
    $local   = Join-Path $Script:TmpDir $archive
    $dest    = Join-Path $Script:TmpDir "skill-src"

    Write-Step "Downloading skill $archive"
    try {
        Invoke-WebRequest -Uri $url         -OutFile $local         -UseBasicParsing -ErrorAction Stop
        Invoke-WebRequest -Uri "$url.sha256" -OutFile "$local.sha256" -UseBasicParsing -ErrorAction Stop
    } catch { Write-Warn "Skill archive unavailable; skipping skill install"; return $null }

    $expected = (Get-Content "$local.sha256" -Raw).Trim().Split()[0].ToLower()
    $actual   = (Get-FileHash $local -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { Write-Warn "Skill checksum mismatch; skipping skill install"; return $null }

    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    # PowerShell's Expand-Archive handles .zip only. Use tar (bundled with
    # Windows 10+ since 2018) to extract the .tar.gz we publish.
    & tar -xzf $local -C $Script:TmpDir
    if ($LASTEXITCODE -ne 0) { Write-Warn "Skill extraction failed; skipping skill install"; return $null }
    return (Join-Path $Script:TmpDir $Script:SkillName)
}

function Install-Skill {
    param([string]$Level, [string]$Source)
    if ($Level -eq "none") { Write-Info "Skill install skipped"; return }
    if (-not (Test-Path $Source)) { Write-Warn "Skill source not found: $Source (skipping)"; return }

    $target = switch ($Level) {
        "user"    { Join-Path $env:USERPROFILE ".claude\skills\$Script:SkillName" }
        "project" { Join-Path (Get-Location) ".claude\skills\$Script:SkillName" }
        default   { Stop-Installer "Invalid skill level: $Level" }
    }

    Write-Step "Installing skill → $target"
    if (Test-Path $target) {
        $existing = Get-SkillVersion -SkillMd (Join-Path $target "SKILL.md")
        $new      = Get-SkillVersion -SkillMd (Join-Path $Source "SKILL.md")
        $cmp      = Compare-SemVer $existing $new
        switch ($cmp) {
            "equal" {
                if (-not $Force -and -not (Read-YesNo "Skill v$existing already installed. Reinstall?" $false)) {
                    Write-Info "Skill kept (v$existing)"; return
                }
            }
            "newer" {
                if (-not $Force -and -not (Read-YesNo "Installed skill (v$existing) is newer than v$new. Downgrade?" $false)) {
                    Write-Info "Skill kept (v$existing)"; return
                }
            }
        }
        Backup-Path $target
        Remove-Item -Path $target -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $target) | Out-Null
    Copy-Item -Path $Source -Destination $target -Recurse -Force
    Write-Ok "Skill installed (v$(Get-SkillVersion (Join-Path $target 'SKILL.md')))"
}

# ═════════════════════════════ ORCHESTRATION ═══════════════════════════════

function Show-Banner {
    param([string]$Platform, [string]$TargetVersion)
    Write-Host ""
    Write-Host "╭──────────────────────────────────────────╮" -ForegroundColor Cyan
    Write-Host "  nodex installer" -ForegroundColor White
    Write-Host "  v$TargetVersion • $Platform" -ForegroundColor DarkGray
    Write-Host "╰──────────────────────────────────────────╯" -ForegroundColor Cyan
}

function Show-Review {
    param([string]$Method, [string]$Dest, [string]$SkillLevel, [string]$TargetVersion)
    Write-Host ""
    Write-Host "Review" -ForegroundColor White
    Write-Host "  binary  $Dest (v$TargetVersion, $Method)"
    switch ($SkillLevel) {
        "user"    { Write-Host "  skill   ~\.claude\skills\$Script:SkillName" }
        "project" { Write-Host "  skill   .\.claude\skills\$Script:SkillName" }
        "none"    { Write-Host "  skill   (skipped)" }
    }
}

function Test-PathMembership {
    param([string]$Dir)
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $segments = ($userPath -split ';') + ($env:PATH -split ';')
    if ($segments -contains $Dir) {
        Write-Ok "$Dir is in PATH"
    } else {
        Write-Warn "$Dir is not in PATH"
        Write-Host "   Add permanently with:" -ForegroundColor DarkGray
        Write-Host "     [Environment]::SetEnvironmentVariable('Path', `"`$env:Path;$Dir`", 'User')" -ForegroundColor DarkGray
    }
}

function Start-Install {
    $Script:TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("nodex-install-" + [Guid]::NewGuid().ToString("N").Substring(0,8))
    New-Item -ItemType Directory -Force -Path $Script:TmpDir | Out-Null

    try {
        $platform = Get-Platform

        $repoDir = $null
        $scriptRoot = try { Split-Path -Parent $PSCommandPath } catch { $null }
        if ($scriptRoot -and (Test-Path (Join-Path $scriptRoot "..\Cargo.toml"))) {
            $repoDir = Resolve-Path (Join-Path $scriptRoot "..") | Select-Object -ExpandProperty Path
        }

        # Method
        if ($Script:ExplicitFromSource -or -not (Test-Interactive)) {
            $method = if ($FromSource) { "source" } else { "prebuilt" }
        } else {
            $idx = Read-Choice -Title "Install method" -Options @("Prebuilt binary (recommended)", "Build from source (requires Rust)") -DefaultIndex 0
            $method = if ($idx -eq 0) { "prebuilt" } else { "source" }
        }

        # Version
        $targetVersion = if ($method -eq "prebuilt") {
            Resolve-Version
        } elseif ($repoDir) {
            (Select-String -Path (Join-Path $repoDir "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
        } else { "dev" }

        Show-Banner -Platform $platform -TargetVersion $targetVersion

        # Install dir (skip prompt when user explicitly overrode)
        if ((Test-Interactive) -and -not $Script:ExplicitInstallDir) {
            $idx = Read-Choice -Title "Install location" -Options @(
                "%USERPROFILE%\.local\bin (recommended)",
                "Custom path"
            ) -DefaultIndex 0
            if ($idx -eq 1) {
                $InstallDir = Read-Path -Question "Install path" -Default $InstallDir
            }
        }
        $dest = Join-Path $InstallDir "$Script:BinaryName.exe"

        # Skill (skip prompt when user explicitly overrode)
        $skillLevel = if ($Script:ExplicitSkill) { $Skill }
            elseif (-not (Test-Interactive)) { "user" }
            else {
                $idx = Read-Choice -Title "Claude Code skill" -Options @(
                    "User-level (~\.claude\skills\$Script:SkillName)",
                    "Project-level (.\.claude\skills\$Script:SkillName)",
                    "Skip"
                ) -DefaultIndex 0
                switch ($idx) { 0 { "user" } 1 { "project" } 2 { "none" } }
            }
        if ($skillLevel -notin @("user", "project", "none")) {
            Stop-Installer "Invalid skill level: $skillLevel (expected user|project|none)"
        }

        Show-Review -Method $method -Dest $dest -SkillLevel $skillLevel -TargetVersion $targetVersion

        if ($DryRun) {
            Write-Host ""
            Write-Host "(dry-run) Not executing" -ForegroundColor Yellow
            return
        }

        if ((Test-Interactive) -and -not (Read-YesNo "Proceed?" $true)) {
            Write-Info "Aborted by user"; return
        }

        # Existing install check
        $skipBinary = $false
        if ((Test-Path $dest) -and -not $Force) {
            $existing = try { (& $dest --version 2>$null).Split()[1] } catch { "" }
            $cmp = Compare-SemVer $existing $targetVersion
            switch ($cmp) {
                "equal" { $skipBinary = -not (Read-YesNo "nodex v$existing already installed. Reinstall?" $false) }
                "newer" { $skipBinary = -not (Read-YesNo "Installed v$existing is newer than v$targetVersion. Downgrade?" $false) }
            }
            if ($skipBinary) { Write-Info "Kept existing install" }
        }

        Write-Host ""

        if (-not $skipBinary) {
            $binarySrc = if ($method -eq "prebuilt") {
                $archive = "$Script:BinaryName-v$targetVersion-$platform.zip"
                Invoke-ReleaseDownload -TargetVersion $targetVersion -Platform $platform -ArchiveName $archive
                Test-Checksum -ArchiveName $archive
                Expand-ReleaseArchive -ArchiveName $archive
                Join-Path $Script:TmpDir "$Script:BinaryName.exe"
            } else {
                if (-not $repoDir) { Stop-Installer "-FromSource requires running from a cloned repo" }
                Build-FromSource -RepoDir $repoDir
            }
            Install-Binary -SourcePath $binarySrc -DestDir $InstallDir
        }

        if ($skillLevel -ne "none") {
            $skillSrc = if ($repoDir -and (Test-Path (Join-Path $repoDir ".claude\skills\$Script:SkillName"))) {
                Join-Path $repoDir ".claude\skills\$Script:SkillName"
            } else {
                Get-SkillTarball -TargetVersion $targetVersion
            }
            if ($skillSrc) { Install-Skill -Level $skillLevel -Source $skillSrc }
        }

        Write-Host ""
        Test-PathMembership -Dir $InstallDir
        Write-Host ""
        Write-Host "✅ Installation complete" -ForegroundColor Green
        Write-Host ""
        Write-Host "Next steps:"
        Write-Host "  nodex init       Initialize nodex.toml"
        Write-Host "  nodex build      Build the document graph"
        Write-Host "  /nodex           Use as Claude Code skill"
    }
    finally {
        if ($Script:TmpDir -and (Test-Path $Script:TmpDir)) {
            Remove-Item -Path $Script:TmpDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

Start-Install
