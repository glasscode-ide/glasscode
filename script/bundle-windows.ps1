<#
.SYNOPSIS
Build Glasscode for Windows: the release programs, the folder target\release\Glasscode, its zip
target\release\Glasscode-<version>-windows-x86_64.zip (the portable copy, and the file an installed
Glasscode updates from), and the installer target\release\Glasscode-<version>-setup.exe.

.DESCRIPTION
  script\bundle-windows.ps1                 # unsigned, for local use and the CI build
  script\bundle-windows.ps1 -NoInstaller    # the programs and the folder only (no Inno Setup)

Windows PowerShell runs a script only when its execution policy lets it:
  powershell -NoProfile -ExecutionPolicy Bypass -File script\bundle-windows.ps1

Signed, when a code signing certificate is at hand:
  $env:SIGN_CERT = 'C:\keys\glasscode.pfx'; $env:SIGN_CERT_PASSWORD = '...'; script\bundle-windows.ps1
SIGN_CERT is a .pfx file, or the SHA-1 thumbprint of a certificate in the current user's
certificate store (where a hardware token puts it). SIGN_TIMESTAMP_URL changes the time stamp
server, SIGNTOOL the signtool.exe of the Windows SDK. Without SIGN_CERT the programs and the
installer are unsigned, and Windows SmartScreen warns before the installer runs.

The installer needs Inno Setup 6.3 or later (https://jrsoftware.org/isinfo.php): ISCC.exe on the
PATH, in the folder of its installer, or at $env:ISCC.

GLASSCODE_BUILD numbers the build, as for the Mac app; without it, the commit count does. The
Linux builds of the server daemon (script/build-glasscoded.sh) go into the folder glasscoded when
they are there.

The folder has the licence texts: LICENSE.txt, NOTICE.txt, the licences of the fonts, and
THIRD-PARTY-NOTICES.html, the licences of every Rust package in the programs. cargo-about writes
that file (cargo install --locked cargo-about --features cli); without it the script warns and
leaves the file out, and with -Publish it stops: a release always has it.
#>
[CmdletBinding()]
param(
    # Build the programs and the folder, without the installer.
    [switch]$NoInstaller,
    # A build to publish: it stops when a part of a release is missing (the third-party notices).
    [switch]$Publish
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# PowerShell does not stop when a program fails: the exit code tells. Windows PowerShell makes an
# error of each line a program writes to stderr when the output is redirected, as cargo's progress
# is in a log; those lines must not stop the script.
function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    $ErrorActionPreference = 'Continue'
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$(Split-Path -Leaf $Program) failed with exit code $LASTEXITCODE"
    }
}

# A program from the environment variable, else from the PATH, else the first of the candidates.
function Find-Tool {
    param([string]$FromEnvironment, [string]$Name, [string[]]$Candidates)
    if ($FromEnvironment) {
        if (-not (Test-Path -LiteralPath $FromEnvironment -PathType Leaf)) {
            throw "$FromEnvironment does not exist"
        }
        return $FromEnvironment
    }
    $onPath = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) {
        return $onPath.Path
    }
    return $Candidates | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
}

function Get-CommitCount {
    # A folder without git history writes to stderr, which must not stop the script.
    $ErrorActionPreference = 'Continue'
    if (Get-Command git -CommandType Application -ErrorAction SilentlyContinue) {
        $count = & git rev-list --count HEAD 2>$null
        if ($LASTEXITCODE -eq 0 -and "$count" -match '^\d+$') {
            return "$count"
        }
    }
    return '1'
}

# The Subsystem field of a program's PE header: 2 is a window program, 3 a console program.
function Get-Subsystem {
    param([string]$Path)
    $header = New-Object byte[] 1024
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        [void]$stream.Read($header, 0, $header.Length)
    } finally {
        $stream.Dispose()
    }
    $pe = [BitConverter]::ToInt32($header, 0x3C)
    # After the signature (4 bytes) and the file header (20 bytes), at offset 68 of the optional header.
    return [BitConverter]::ToUInt16($header, $pe + 4 + 20 + 68)
}

function Invoke-Sign {
    param([string[]]$Files)
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $fromSdk = @(Get-ChildItem -Path (Join-Path $kits '*\x64\signtool.exe') -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | ForEach-Object { $_.FullName })
    $signtool = Find-Tool $env:SIGNTOOL 'signtool.exe' $fromSdk
    if (-not $signtool) {
        throw 'signtool.exe was not found: install the Windows SDK, or set SIGNTOOL'
    }
    $timestamp = if ($env:SIGN_TIMESTAMP_URL) { $env:SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    $arguments = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $timestamp)
    if (Test-Path -LiteralPath $env:SIGN_CERT -PathType Leaf) {
        $arguments += @('/f', $env:SIGN_CERT)
        if ($env:SIGN_CERT_PASSWORD) {
            $arguments += @('/p', $env:SIGN_CERT_PASSWORD)
        }
    } elseif ($env:SIGN_CERT -match '^[0-9A-Fa-f]{40}$') {
        $arguments += @('/sha1', $env:SIGN_CERT)
    } else {
        throw 'SIGN_CERT is neither a .pfx file nor the SHA-1 thumbprint of a certificate'
    }
    Invoke-Checked $signtool ($arguments + $Files)
}

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # The version of the workspace ([workspace.package]): the only line of Cargo.toml that starts so.
    $line = Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $line) {
        throw 'Cargo.toml has no version'
    }
    $version = $line.Matches[0].Groups[1].Value
    if (-not $env:GLASSCODE_BUILD) {
        $env:GLASSCODE_BUILD = Get-CommitCount
    }
    Write-Host "Building Glasscode $version (build $env:GLASSCODE_BUILD)"
    Invoke-Checked 'cargo' @('build', '--release', '--locked', '-p', 'glasscode', '-p', 'glasscode_cli')

    $release = Join-Path $root 'target\release'
    $stage = Join-Path $release 'Glasscode'
    if (Test-Path -LiteralPath $stage) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stage | Out-Null
    # The installer and the self-update (update::windows_app) know the app by this name.
    $app = Join-Path $stage 'Glasscode.exe'
    Copy-Item -LiteralPath (Join-Path $release 'glasscode.exe') -Destination $app
    # The app expects the console program next to itself (cli::console_program).
    $console = Join-Path $stage 'glasscode-cli.exe'
    Copy-Item -LiteralPath (Join-Path $release 'glasscode-cli.exe') -Destination $console
    $daemons = @(Get-ChildItem -Path (Join-Path $root 'target\glasscoded') -Filter 'glasscoded-linux-*' -File -ErrorAction SilentlyContinue)
    if ($daemons.Count -gt 0) {
        $folder = New-Item -ItemType Directory -Path (Join-Path $stage 'glasscoded')
        $daemons | Copy-Item -Destination $folder.FullName
    } else {
        Write-Warning 'no glasscoded Linux builds (run script/build-glasscoded.sh); the app cannot install the server daemon'
    }

    # The licence texts go with the programs (Apache-2.0, section 4; the fonts' OFL).
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $stage 'LICENSE.txt')
    Copy-Item -LiteralPath (Join-Path $root 'NOTICE') -Destination (Join-Path $stage 'NOTICE.txt')
    $licenses = New-Item -ItemType Directory -Path (Join-Path $stage 'licenses')
    $fonts = New-Item -ItemType Directory -Path (Join-Path $licenses.FullName 'fonts')
    Get-ChildItem -Path (Join-Path $root 'crates\glasscode\assets\fonts') -Filter '*.txt' -File | Copy-Item -Destination $fonts.FullName
    Copy-Item -LiteralPath (Join-Path $root 'vendor\LICENSE-APACHE') -Destination (Join-Path $licenses.FullName 'GPUI-LICENSE-APACHE.txt')
    $notices = Join-Path $stage 'THIRD-PARTY-NOTICES.html'
    $ErrorActionPreference = 'Continue'
    & cargo about --version *> $null
    $hasAbout = $LASTEXITCODE -eq 0
    $ErrorActionPreference = 'Stop'
    if ($hasAbout) {
        # The packages of what the folder has: the two programs for Windows, and the server
        # daemon for Linux. Not those of the Mac app.
        Invoke-Checked 'cargo' @('about', 'generate', '--locked', '--fail', '--workspace', '--target', 'x86_64-pc-windows-msvc', '--target', 'x86_64-unknown-linux-gnu', '--target', 'aarch64-unknown-linux-gnu', '-o', $notices, 'about.hbs')
    } elseif ($Publish) {
        throw 'cargo-about is not installed (cargo install --locked cargo-about --features cli): a release needs THIRD-PARTY-NOTICES.html'
    } else {
        Write-Warning 'cargo-about is not installed: the folder has no THIRD-PARTY-NOTICES.html (cargo install --locked cargo-about --features cli)'
    }

    # A release of the app opens no console window; the command is a console program.
    if ((Get-Subsystem $app) -ne 2) {
        throw "$app is not a window program: the release build must use the Windows subsystem"
    }
    if ((Get-Subsystem $console) -ne 3) {
        throw "$console is not a console program"
    }
    $info = (Get-Item -LiteralPath $app).VersionInfo
    if ($info.ProductName -ne 'Glasscode' -or $info.FileVersion -ne $version) {
        throw "$app has no version resource for Glasscode $version (crates\glasscode\build.rs)"
    }

    if ($env:SIGN_CERT) {
        Invoke-Sign @($app, $console)
    } else {
        Write-Warning 'SIGN_CERT is not set: the programs and the installer are unsigned, and Windows SmartScreen warns before the installer runs'
    }

    # The zip has the programs at its top: update::windows_app::stage unpacks it so. The tar.exe
    # of Windows writes it, as it reads it there.
    $zip = Join-Path $release "Glasscode-$version-windows-x86_64.zip"
    if (Test-Path -LiteralPath $zip) {
        Remove-Item -LiteralPath $zip -Force
    }
    Invoke-Checked (Join-Path $env:SystemRoot 'System32\tar.exe') @('-a', '-cf', $zip, '-C', $stage, '.')

    if ($NoInstaller) {
        Write-Host "Built $stage and $zip (no installer: -NoInstaller)"
        return
    }
    $innoFolders = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, (Join-Path $env:LOCALAPPDATA 'Programs')) | Where-Object { $_ }
    $iscc = Find-Tool $env:ISCC 'ISCC.exe' @($innoFolders | ForEach-Object { Join-Path $_ 'Inno Setup 6\ISCC.exe' })
    if (-not $iscc) {
        throw 'Inno Setup (ISCC.exe) was not found: install Inno Setup 6.3 or later from https://jrsoftware.org/isinfo.php, set ISCC, or use -NoInstaller'
    }
    $installer = Join-Path $release "Glasscode-$version-setup.exe"
    if (Test-Path -LiteralPath $installer) {
        Remove-Item -LiteralPath $installer -Force
    }
    Invoke-Checked $iscc @("/DAppVersion=$version", "/DAppBuild=$env:GLASSCODE_BUILD", "/DStageDir=$stage", "/DOutputDir=$release", (Join-Path $PSScriptRoot 'windows\installer.iss'))
    if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
        throw "Inno Setup made no $installer"
    }
    if ($env:SIGN_CERT) {
        Invoke-Sign @($installer)
    }
    Write-Host "Built $stage, $zip and $installer"
} finally {
    Pop-Location
}
