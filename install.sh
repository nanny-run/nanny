#!/usr/bin/env sh
# Install the Nanny CLI
# Usage: curl -fsSL https://install.nanny.run | sh
set -e

REPO="nanny-run/nanny"
BINARY="nanny"

# ── Detect OS ─────────────────────────────────────────────────────────────────
case "$(uname -s)" in
  Darwin) OS="macos" ;;
  Linux)  OS="linux" ;;
  *)
    echo "error: unsupported OS: $(uname -s)" >&2
    echo "Install via cargo instead: cargo install nannyd" >&2
    exit 1
    ;;
esac

# ── Detect architecture ───────────────────────────────────────────────────────
case "$(uname -m)" in
  arm64|aarch64) ARCH="arm64"  ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *)
    echo "error: unsupported architecture: $(uname -m)" >&2
    echo "Install via cargo instead: cargo install nannyd" >&2
    exit 1
    ;;
esac

ARTIFACT="nanny-${OS}-${ARCH}"
URL="https://github.com/${REPO}/releases/latest/download/${ARTIFACT}.tar.gz"

# ── Choose install directory ──────────────────────────────────────────────────
if [ -w /usr/local/bin ]; then
  INSTALL_DIR="/usr/local/bin"
else
  INSTALL_DIR="${HOME}/.local/bin"
  mkdir -p "${INSTALL_DIR}"
fi

# ── Download and install ──────────────────────────────────────────────────────
TMP=$(mktemp -d)
trap 'rm -rf "${TMP}"' EXIT

echo "Downloading ${ARTIFACT}..."

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "${URL}" | tar xz -C "${TMP}"
elif command -v wget >/dev/null 2>&1; then
  wget -qO- "${URL}" | tar xz -C "${TMP}"
else
  echo "error: curl or wget is required" >&2
  exit 1
fi

install -m 755 "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"

echo ""
echo "nanny installed to ${INSTALL_DIR}/${BINARY}"
echo ""

# ── PATH reminder ─────────────────────────────────────────────────────────────
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "NOTE: Add ${INSTALL_DIR} to your PATH:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    ;;
esac

echo "Run 'nanny --version' to confirm."
