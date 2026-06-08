#!/usr/bin/env bash
set -euo pipefail

repo="${CODEX_TOOLS_REPO:-simplaj/Codex-API-Tools}"
version="${CODEX_TOOLS_VERSION:-latest}"
install_dir="${CODEX_TOOLS_INSTALL_DIR:-}"

usage() {
  cat <<'EOF'
Install the codex-tools CLI on macOS or Linux.

Usage:
  install-codex-tools.sh [--version TAG] [--install-dir DIR] [--repo OWNER/REPO]

Examples:
  curl -fsSL https://github.com/simplaj/Codex-API-Tools/releases/latest/download/install-codex-tools.sh | bash
  CODEX_TOOLS_INSTALL_DIR=/usr/local/bin bash install-codex-tools.sh
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2:-}"
      shift 2
      ;;
    --install-dir)
      install_dir="${2:-}"
      shift 2
      ;;
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$repo" || -z "$version" || -z "$install_dir" ]]; then
  if [[ -z "$repo" || -z "$version" ]]; then
    echo "repo and version must not be empty" >&2
    exit 2
  fi
fi

path_contains() {
  local dir="$1"
  case ":$PATH:" in
    *":$dir:"*) return 0 ;;
    *) return 1 ;;
  esac
}

is_writable_or_creatable() {
  local dir="$1"
  if [[ -d "$dir" && -w "$dir" ]]; then
    return 0
  fi
  local parent
  parent="$(dirname "$dir")"
  [[ -d "$parent" && -w "$parent" ]]
}

resolve_install_dir() {
  if [[ -n "$install_dir" ]]; then
    echo "$install_dir"
    return
  fi

  local candidate
  for candidate in "$HOME/.local/bin" "$HOME/bin" "/usr/local/bin" "/opt/homebrew/bin"; do
    if path_contains "$candidate" && is_writable_or_creatable "$candidate"; then
      echo "$candidate"
      return
    fi
  done

  IFS=":" read -r -a path_dirs <<< "$PATH"
  for candidate in "${path_dirs[@]}"; do
    if [[ -n "$candidate" ]] && is_writable_or_creatable "$candidate"; then
      echo "$candidate"
      return
    fi
  done

  if path_contains "/usr/local/bin" && command -v sudo >/dev/null 2>&1; then
    echo "/usr/local/bin"
    return
  fi

  echo "$HOME/.local/bin"
}

install_dir="$(resolve_install_dir)"
if [[ -z "$install_dir" ]]; then
  echo "install dir must not be empty" >&2
  exit 2
fi

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin)
    platform="macos"
    case "$arch" in
      arm64|aarch64) asset="codex-tools-macos-aarch64" ;;
      x86_64|amd64) asset="codex-tools-macos-x64" ;;
      *) echo "unsupported macOS architecture: $arch" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$arch" in
      x86_64|amd64)
        platform="linux"
        asset="codex-tools-linux-x64"
        ;;
      *)
        echo "unsupported Linux architecture: $arch" >&2
        echo "Only Linux x64 is published right now." >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "unsupported OS: $os" >&2
    exit 1
    ;;
esac

if [[ "$version" == "latest" ]]; then
  base_url="https://github.com/$repo/releases/latest/download"
else
  base_url="https://github.com/$repo/releases/download/$version"
fi

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

binary_path="$tmp_dir/codex-tools"
checksum_path="$tmp_dir/$asset.sha256.txt"

download() {
  local url="$1"
  local output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$output"
  else
    echo "curl or wget is required to download codex-tools" >&2
    exit 1
  fi
}

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required to verify codex-tools" >&2
    exit 1
  fi
}

echo "Downloading $asset for $platform..."
download "$base_url/$asset" "$binary_path"
download "$base_url/$asset.sha256.txt" "$checksum_path"

expected="$(awk '{print $1}' "$checksum_path")"
actual="$(sha256_file "$binary_path")"
if [[ "$expected" != "$actual" ]]; then
  echo "checksum mismatch for $asset" >&2
  echo "expected: $expected" >&2
  echo "actual:   $actual" >&2
  exit 1
fi

if is_writable_or_creatable "$install_dir"; then
  mkdir -p "$install_dir"
  cp "$binary_path" "$install_dir/codex-tools"
  chmod +x "$install_dir/codex-tools"
elif command -v sudo >/dev/null 2>&1; then
  sudo mkdir -p "$install_dir"
  sudo cp "$binary_path" "$install_dir/codex-tools"
  sudo chmod +x "$install_dir/codex-tools"
else
  echo "$install_dir is not writable. Re-run with CODEX_TOOLS_INSTALL_DIR set to a writable PATH directory." >&2
  exit 1
fi

echo "Installed codex-tools to $install_dir/codex-tools"

if ! path_contains "$install_dir"; then
  echo "Add this directory to PATH if your shell cannot find codex-tools:"
  echo "  export PATH=\"$install_dir:\$PATH\""
fi

"$install_dir/codex-tools" --help >/dev/null
echo "Run: codex-tools cloud login --email user@example.com"
