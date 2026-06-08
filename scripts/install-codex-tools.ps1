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

$Asset = "codex-tools-windows-x64.exe"
if ($Version -eq "latest") {
  $BaseUrl = "https://github.com/$Repo/releases/latest/download"
} else {
  $BaseUrl = "https://github.com/$Repo/releases/download/$Version"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("codex-tools-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

try {
  $DownloadPath = Join-Path $TempDir "codex-tools.exe"
  $ChecksumPath = Join-Path $TempDir "$Asset.sha256.txt"

  Write-Host "Downloading $Asset..."
  Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $DownloadPath
  Invoke-WebRequest -Uri "$BaseUrl/$Asset.sha256.txt" -OutFile $ChecksumPath

  $Expected = ((Get-Content $ChecksumPath -Raw).Trim() -split "\s+")[0].ToLowerInvariant()
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

  & $Target --help | Out-Null
  Write-Host "Installed codex-tools to $Target"
  Write-Host "Run: codex-tools cloud login --email user@example.com"
} finally {
  Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}
