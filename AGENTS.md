# Vibe: Project Context & Instructions

Vibe is a lightweight, zero-configuration tool designed to spin up Linux virtual machines on ARM-based Macs to sandbox LLM agents. It prioritizes speed, security, and a minimal footprint.

## Project Overview

- **Purpose:** Provides a secure, isolated environment for LLM agents (like Claude Code, Codex, or Gemini CLI) to run commands without risking the host system.
- **Core Technology:** Built with **Rust** using Apple's **Virtualization framework** (via `objc2` interop).
- **Guest OS:** Uses a **Debian Linux** (nocloud) base image, optimized for fast boot (~10s).
- **Architecture:**
    - **Single Binary:** The entire tool is a single binary (< 1MB).
    - **Direct Console:** Attaches the host terminal directly to the VM's serial console.
    - **Directory Sharing:** Uses **VirtioFS** for high-performance directory sharing between host and guest.
    - **Networking:** Includes a bundled C-based `vmnet-helper` (in `vendor/vmnet-helper`) for reliable NAT and bridged networking, bypassing limitations in macOS's default virtualization networking.
- **Security:** Automatically signs itself with the `com.apple.security.virtualization` entitlement if missing.

## Key Files & Directories

- `src/main.rs`: Entry point, CLI parsing (via `lexopt`), VM configuration, and IO multiplexing.
- `src/networking.rs`: Logic for setting up NAT, bridged, and `vmnet-helper` networking backends.
- `src/provision.sh`: Shell script used to initialize the default Debian image with essential tools (gcc, rust, mise, etc.).
- `src/bashrc.sh`: Guest-side bash configuration.
- `src/entitlements.plist`: Entitlements required for macOS virtualization.
- `build.rs`: Compiles the bundled `vmnet-helper` and embeds version info.
- `vendor/vmnet-helper/`: Vendored C code for the networking helper.

## Building and Running

### Prerequisites
- ARM-based Mac (M1/M2/M3/M4).
- macOS 13 (Ventura) or higher.
- `codesign` tool (part of Xcode/Command Line Tools).

### Commands
- **Build:** `cargo build --release`
- **Run:** `cargo run -- [OPTIONS] [disk-image.raw]`
- **Test:** `cargo test` (Note: Most logic is integration-heavy and requires a Mac host).

## Development Conventions

- **Minimal Dependencies:** Maintain the "small binary" philosophy. Avoid adding heavy crates. Current dependencies are primarily `objc2`, `lexopt`, `libc`, and `regex`.
- **Senior Engineering Standards:** Code should be clean, idiomatic Rust, and follow existing patterns (e.g., surgical `objc2` usage).
- **No Emoji:** The CLI and documentation avoid emoji to maintain a professional, minimalist aesthetic.
- **Human-Centric:** Documentation (like the README) is written by humans for humans.
- **macOS Interop:** Be mindful of macOS-specific behaviors, especially entitlements and security boundaries.

## Common Tasks

- **Adding a Default Mount:** Modify `main.rs` in the `directory_shares` section of `main()`.
- **Updating Guest Provisioning:** Edit `src/provision.sh`. Note that this only affects the *first* run when `default.raw` is created.
- **Adjusting Networking:** Look into `src/networking.rs` and the `NetworkMode` enum.
- **Fixing Entitlement Issues:** `main.rs` contains an `ensure_signed()` function that handles self-signing and re-execution.
