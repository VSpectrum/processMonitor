#!/usr/bin/env bash
# ==============================================================================
# build.sh — macOS Build & Packaging Script for procmon
# ==============================================================================
# Compiles an optimized, standalone release binary for macOS, applies ad-hoc
# code signing, places the executable in ./bin/procmon & ./procmon, and
# optionally installs it directly into your $PATH (~/.cargo/bin or /usr/local/bin).
# ==============================================================================

set -euo pipefail

# Colors & Styling
BOLD="\033[1m"
GREEN="\033[0;32m"
CYAN="\033[0;36m"
YELLOW="\033[0;33m"
RED="\033[0;31m"
RESET="\033[0m"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_NAME="procmon"
OUTPUT_DIR="${SCRIPT_DIR}/bin"
INSTALL_DIR="${HOME}/.cargo/bin"
DO_INSTALL=false
DO_CLEAN=false
DO_UNIVERSAL=false

# ------------------------------------------------------------------------------
# Usage & Help
# ------------------------------------------------------------------------------
show_help() {
    cat <<EOF
${BOLD}PROCMON — macOS Standalone Build Script${RESET}

${BOLD}Usage:${RESET}
  ./build.sh [OPTIONS]

${BOLD}Options:${RESET}
  -i, --install         Install the built binary to ${INSTALL_DIR}
                        (enables running 'procmon' directly from any terminal)
  --install-dir <DIR>   Specify custom installation directory
  -u, --universal       Build a Universal macOS binary (Apple Silicon arm64 + Intel x86_64)
  -c, --clean           Perform 'cargo clean' before building
  -h, --help            Display this help message and exit

${BOLD}Examples:${RESET}
  ./build.sh                  # Build optimized release binary in ./bin/procmon and ./procmon
  ./build.sh --install        # Build and install to ~/.cargo/bin (in your \$PATH)
  ./build.sh -u               # Build universal binary for distribution to any Mac
EOF
}

# ------------------------------------------------------------------------------
# Argument Parsing
# ------------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            show_help
            exit 0
            ;;
        -i|--install)
            DO_INSTALL=true
            shift
            ;;
        --install-dir)
            INSTALL_DIR="$2"
            DO_INSTALL=true
            shift 2
            ;;
        -u|--universal)
            DO_UNIVERSAL=true
            shift
            ;;
        -c|--clean)
            DO_CLEAN=true
            shift
            ;;
        *)
            echo -e "${RED}Error:${RESET} Unknown option: $1"
            echo "Run './build.sh --help' for usage."
            exit 1
            ;;
    esac
done

# ------------------------------------------------------------------------------
# System & Dependency Verification
# ------------------------------------------------------------------------------
echo -e "${BOLD}${CYAN}==> Verifying build environment...${RESET}"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo -e "${RED}Error:${RESET} procmon uses macOS-specific libproc kernel APIs and must be built on macOS."
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error:${RESET} 'cargo' was not found in your PATH."
    echo "Install Rust via https://rustup.rs or ensure ~/.cargo/bin is in your PATH."
    exit 1
fi

if ! command -v rustc &> /dev/null; then
    echo -e "${RED}Error:${RESET} 'rustc' was not found in your PATH."
    exit 1
fi

HOST_ARCH="$(uname -m)"
echo -e "  Host OS:       macOS $(sw_vers -productVersion 2>/dev/null || echo '') ($(uname -s))"
echo -e "  Architecture:  ${HOST_ARCH}"
echo -e "  Rust Version:  $(rustc --version)"

# ------------------------------------------------------------------------------
# Optional Clean
# ------------------------------------------------------------------------------
cd "${SCRIPT_DIR}"

if [[ "$DO_CLEAN" == true ]]; then
    echo -e "\n${BOLD}${CYAN}==> Cleaning previous build artifacts...${RESET}"
    cargo clean
    rm -rf "${OUTPUT_DIR}" "${SCRIPT_DIR}/${BIN_NAME}"
fi

mkdir -p "${OUTPUT_DIR}"

# ------------------------------------------------------------------------------
# Compilation
# ------------------------------------------------------------------------------
if [[ "$DO_UNIVERSAL" == true ]]; then
    echo -e "\n${BOLD}${CYAN}==> Building Universal macOS binary (arm64 + x86_64)...${RESET}"
    
    # Ensure targets are installed
    echo -e "  Checking rustup targets..."
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    
    echo -e "  Compiling for Apple Silicon (aarch64-apple-darwin)..."
    cargo build --release --target aarch64-apple-darwin
    
    echo -e "  Compiling for Intel (x86_64-apple-darwin)..."
    cargo build --release --target x86_64-apple-darwin
    
    echo -e "  Stitching universal binary with lipo..."
    lipo -create -output "${OUTPUT_DIR}/${BIN_NAME}" \
        "${SCRIPT_DIR}/target/aarch64-apple-darwin/release/${BIN_NAME}" \
        "${SCRIPT_DIR}/target/x86_64-apple-darwin/release/${BIN_NAME}"
else
    echo -e "\n${BOLD}${CYAN}==> Compiling optimized release binary...${RESET}"
    cargo build --release
    cp "${SCRIPT_DIR}/target/release/${BIN_NAME}" "${OUTPUT_DIR}/${BIN_NAME}"
fi

FINAL_BIN="${OUTPUT_DIR}/${BIN_NAME}"
chmod +x "${FINAL_BIN}"

# ------------------------------------------------------------------------------
# Ad-Hoc Code Signing (macOS Gatekeeper & Kernel API hygiene)
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}${CYAN}==> Code signing executable (ad-hoc)...${RESET}"
if command -v codesign &> /dev/null; then
    codesign --force --sign - "${FINAL_BIN}"
    echo -e "  ${GREEN}✓${RESET} Ad-hoc signature successfully applied."
else
    echo -e "  ${YELLOW}!${RESET} 'codesign' tool not found, skipping code signing."
fi

# ------------------------------------------------------------------------------
# Create convenient root symlink
# ------------------------------------------------------------------------------
ROOT_SYMLINK="${SCRIPT_DIR}/${BIN_NAME}"
ln -sf "bin/${BIN_NAME}" "${ROOT_SYMLINK}"

# ------------------------------------------------------------------------------
# Verification Check
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}${CYAN}==> Verifying binary...${RESET}"
if "${FINAL_BIN}" --help > /dev/null 2>&1; then
    echo -e "  ${GREEN}✓${RESET} Self-test passed: binary executes cleanly."
else
    echo -e "  ${RED}✗${RESET} Warning: binary self-test returned non-zero code."
fi

BIN_SIZE=$(ls -lh "${FINAL_BIN}" | awk '{print $5}')
BIN_ARCH=$(file "${FINAL_BIN}" | sed "s/.*: //")

# ------------------------------------------------------------------------------
# Optional Installation
# ------------------------------------------------------------------------------
if [[ "$DO_INSTALL" == true ]]; then
    echo -e "\n${BOLD}${CYAN}==> Installing to ${INSTALL_DIR}...${RESET}"
    mkdir -p "${INSTALL_DIR}"
    cp "${FINAL_BIN}" "${INSTALL_DIR}/${BIN_NAME}"
    chmod +x "${INSTALL_DIR}/${BIN_NAME}"
    echo -e "  ${GREEN}✓${RESET} Successfully installed to ${INSTALL_DIR}/${BIN_NAME}"
fi

# ------------------------------------------------------------------------------
# Build Summary
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}${GREEN}================================================================${RESET}"
echo -e "${BOLD}${GREEN}  BUILD COMPLETE: procmon is ready!${RESET}"
echo -e "${BOLD}${GREEN}================================================================${RESET}"
echo -e "  Executable:     ${BOLD}${FINAL_BIN}${RESET}"
echo -e "  Local Link:     ${BOLD}${ROOT_SYMLINK}${RESET}"
echo -e "  Binary Size:    ${BOLD}${BIN_SIZE}${RESET}"
echo -e "  Mach-O Type:    ${BIN_ARCH}"
echo -e ""
echo -e "${BOLD}How to run:${RESET}"
echo -e "  ${CYAN}./procmon${RESET}              # Run directly from this directory"
echo -e "  ${CYAN}./procmon -i 2 -n 10${RESET}   # 2-second interval, top 10 processes"
echo -e "  ${CYAN}sudo ./procmon${RESET}         # Elevated privileges (inspect root daemons)"
if [[ "$DO_INSTALL" == true ]]; then
    echo -e "  ${CYAN}procmon${RESET}                # Run from anywhere in any terminal"
else
    echo -e ""
    echo -e "${BOLD}Tip:${RESET} Run ${CYAN}./build.sh --install${RESET} to install to ${INSTALL_DIR} so you can run 'procmon' from anywhere without './'."
fi
echo -e "${BOLD}${GREEN}================================================================${RESET}\n"
