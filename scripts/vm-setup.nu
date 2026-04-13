#!/usr/bin/env nu
# Bootstrap a fresh Ubuntu VM for nono development.
#
# Usage: nu scripts/vm-setup.nu [--verbose]
#
# Security notes:
# - Installers are downloaded to disk before execution (no curl|sh)
# - Rustup installer is sha256-verified

use std/assert

# ─── Constants ────────────────────────────────────────────────────────────────

const UBUNTU_CODENAME = "questing"
const ARCH = "amd64"

const RUSTUP_INSTALL_URL = "https://sh.rustup.rs"
const RUSTUP_INSTALL_SHA256 = "6c30b75a75b28a96fd913a037c8581b580080b6ee9b8169a3c0feb1af7fe8caf"

const APT_PACKAGES = [
  build-essential
  libdbus-1-dev
  pkg-config
]

const PROFILE_LINES = [
  'source "$HOME/.cargo/env"'
  'export CARGO_TARGET_DIR="$HOME/.cargo-target/nono"'
  'export GIT_TERMINAL_PROMPT=0'
  'cd ~/nono'
]

const BASHRC_LINES = [
  'alias cls=clear'
]

# ─── Helpers ──────────────────────────────────────────────────────────────────

def step [msg: string] {
  print $"(ansi cyan)==>(ansi reset) ($msg)"
}

def has-cmd [name: string]: nothing -> bool {
  which $name | is-not-empty
}

# Run a closure, swallowing stdout/stderr unless --verbose was passed.
def quiet [verbose: bool, action: closure] {
  if $verbose { do $action } else { do $action o+e> /dev/null }
}

# Append a line to a file only if the file doesn't already contain it.
def ensure-line [path: string, line: string] {
  let present = if ($path | path exists) {
    open --raw $path | str contains $line
  } else {
    false
  }
  if not $present {
    $"($line)\n" | save --append $path
  }
}

# Download a URL to a temp file, sha256-verify if expected hash given,
# run the closure with the file path, and clean up no matter what.
def with-downloaded [
  url: string
  action: closure
  --sha256: string
] {
  let tmp = mktemp | str trim
  try {
    http get --raw $url | save --raw --force $tmp

    if $sha256 != null {
      let actual = open --raw $tmp | hash sha256
      if $actual != $sha256 {
        error make {
          msg: $"sha256 mismatch: expected ($sha256), got ($actual)"
        }
      }
    }

    do $action $tmp
  } catch { |e|
    rm -f $tmp
    error make { msg: $e.msg }
  }
  rm -f $tmp
}

# ─── Install steps ────────────────────────────────────────────────────────────

def install-system-packages [verbose: bool] {
  step "Updating system packages"
  quiet $verbose { sudo apt-get update -qq }
  quiet $verbose { sudo DEBIAN_FRONTEND=noninteractive apt-get upgrade -y }

  step $"Installing build deps: ($APT_PACKAGES | str join ', ')"
  quiet $verbose { sudo apt-get install -y ...$APT_PACKAGES }
}

def --env install-rust [verbose: bool, home: string] {
  step "Installing Rust"
  if (has-cmd rustc) {
    print $"    already installed: (^rustc --version)"
    return
  }

  with-downloaded $RUSTUP_INSTALL_URL --sha256 $RUSTUP_INSTALL_SHA256 {|installer|
    quiet $verbose { ^sh $installer -y --default-toolchain stable }
  }

  $env.PATH = $env.PATH | prepend $"($home)/.cargo/bin"
}

def install-cargo-tools [verbose: bool] {
  step "Installing cargo-audit"
  if (has-cmd cargo-audit) {
    print $"    already installed: (^cargo-audit --version)"
    return
  }

  quiet $verbose { ^cargo install cargo-audit }
}

def --env configure-git [] {
  step "Configuring git"
  ^git config --global credential.helper ''
  $env.GIT_TERMINAL_PROMPT = "0"
}

def configure-shell [home: string] {
  step "Suppressing login banner"
  touch $"($home)/.hushlogin"

  step "Writing shell config"
  $PROFILE_LINES | each { |line| ensure-line $"($home)/.profile" $line } | ignore
  $BASHRC_LINES  | each { |line| ensure-line $"($home)/.bashrc"  $line } | ignore
}

def verify-installation [] {
  step "Verifying"
  {
    rustc:       (^rustc --version       | split row ' ' | get 1)
    cargo:       (^cargo --version       | split row ' ' | get 1)
    cargo-audit: (^cargo-audit --version | split row ' ' | get 1)
  }
  | items { |name, ver| { tool: $name, version: $ver, path: (which $name | get path.0) } }
  | table
  | print

  step "Checking Landlock support"
  # nono isn't installed yet (we're building from source), just check kernel
  let ll = ^cat /sys/kernel/security/lsm | str trim
  if ($ll | str contains "landlock") {
    print "    Landlock enabled in kernel"
  } else {
    print "    WARNING: Landlock not found in kernel LSMs"
  }
}

# ─── Entry point ──────────────────────────────────────────────────────────────

def main [--verbose (-v)] {
  let actual_arch = ^dpkg --print-architecture | str trim
  assert ($actual_arch == $ARCH) $"this script targets ($ARCH), got ($actual_arch)"
  assert (open /etc/os-release | str contains $"VERSION_CODENAME=($UBUNTU_CODENAME)") $"this script targets Ubuntu ($UBUNTU_CODENAME)"

  let started = date now
  let home = $env.HOME

  install-system-packages $verbose
  install-rust $verbose $home
  install-cargo-tools $verbose
  configure-git
  configure-shell $home
  verify-installation

  let elapsed = (date now) - $started
  step $"Done in ($elapsed)"
}
