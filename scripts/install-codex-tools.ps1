param(
  [string]$Version = "latest",
  [string]$InstallDir = "$env:LOCALAPPDATA\CodexTools\bin",
  [string]$Repo = "simplaj/Codex-API-Tools",
  [switch]$NoPathUpdate
)

$ErrorActionPreference = "Stop"

if (-not $Repo -or -not $Version -or -not $InstallDir) {
  throw "Repo, Version, and InstallDir must not be empty."
}

try {
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
  # PowerShell Core on newer Windows versions may not need ServicePointManager.
}

function Invoke-CodexToolsDownload {
  param(
    [Parameter(Mandatory = $true)][string]$Uri,
    [Parameter(Mandatory = $true)][string]$OutFile
  )

  $Params = @{
    Uri = $Uri
    OutFile = $OutFile
  }
  if ((Get-Command Invoke-WebRequest).Parameters.ContainsKey("UseBasicParsing")) {
    $Params.UseBasicParsing = $true
  }
  Invoke-WebRequest @Params
}

$Asset = "codex-tools-cli-windows-x64.exe"
if ($Version -eq "latest") {
  $BaseUrl = "https://github.com/$Repo/releases/latest/download"
} else {
  $BaseUrl = "https://github.com/$Repo/releases/download/$Version"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("codex-tools-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

try {
  $DownloadPath = Join-Path $TempDir "codex-tools.exe"
  $ChecksumPath = Join-Path $TempDir "codex-tools-cli-checksums.txt"

  Write-Host "Downloading $Asset..."
  Invoke-CodexToolsDownload -Uri "$BaseUrl/$Asset" -OutFile $DownloadPath
  Invoke-CodexToolsDownload -Uri "$BaseUrl/codex-tools-cli-checksums.txt" -OutFile $ChecksumPath
  $Expected = $null
  foreach ($Line in Get-Content $ChecksumPath) {
    $Parts = $Line.Trim() -split "\s+"
    if ($Parts.Length -ge 2 -and $Parts[1] -eq $Asset) {
      $Expected = $Parts[0].ToLowerInvariant()
      break
    }
  }
  if (-not $Expected) {
    throw "Checksum for $Asset was not found in codex-tools-cli-checksums.txt."
  }
  $Actual = (Get-FileHash -Algorithm SHA256 $DownloadPath).Hash.ToLowerInvariant()
  if ($Expected -ne $Actual) {
    throw "Checksum mismatch for $Asset. Expected $Expected, got $Actual."
  }

  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  $Target = Join-Path $InstallDir "codex-tools.exe"
  Copy-Item $DownloadPath $Target -Force

  if (-not $NoPathUpdate) {
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathParts = @()
    if ($UserPath) {
      $PathParts = $UserPath.Split(";") | Where-Object { $_ }
    }
    $AlreadyInPath = $false
    foreach ($Part in $PathParts) {
      if ($Part.TrimEnd("\") -ieq $InstallDir.TrimEnd("\")) {
        $AlreadyInPath = $true
        break
      }
    }
    if (-not $AlreadyInPath) {
      $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
      [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
      $env:Path = "$env:Path;$InstallDir"
      Write-Host "Added $InstallDir to the current user's PATH. Open a new terminal if codex-tools is not found."
    }
  }

  $CurrentPathParts = @()
  if ($env:Path) {
    $CurrentPathParts = $env:Path.Split(";") | Where-Object { $_ }
  }
  $AlreadyInCurrentPath = $false
  foreach ($Part in $CurrentPathParts) {
    if ($Part.TrimEnd("\") -ieq $InstallDir.TrimEnd("\")) {
      $AlreadyInCurrentPath = $true
      break
    }
  }
  if (-not $AlreadyInCurrentPath) {
    $env:Path = "$env:Path;$InstallDir"
  }

  & $Target --help | Out-Null
  Write-Host "Installed codex-tools to $Target"
  Write-Host "Run: codex-tools cloud login --email user@example.com"
} finally {
  Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}
