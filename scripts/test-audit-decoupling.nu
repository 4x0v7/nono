#!/usr/bin/env nu
# Validate issue #269: audit trail works independently of rollback.
#
# Usage: nu scripts/test-audit-decoupling.nu
#
# Requires: nono debug binary at ~/.cargo-target/nono/debug/nono

use std/assert

def step [msg: string] {
  print $"(ansi cyan)==>(ansi reset) ($msg)"
}

# List session dirs, sorted oldest first.
def sessions-snapshot [root: string] {
  if not ($root | path exists) { return [] }
  ls $root | where type == dir | get name | sort
}

# Return session dirs that appeared after a baseline snapshot.
def new-sessions [root: string, before: list] {
  let after = sessions-snapshot $root
  $after | where { |d| $d not-in $before }
}

# Check whether a session dir contains a session.json metadata file.
def has-session-json [dir: string] {
  ($"($dir)/session.json" | path exists)
}

# Check whether a session dir contains snapshot/rollback data.
def has-snapshots [dir: string] {
  ($"($dir)/snapshots" | path exists)
}

def test-dir [] {
  mktemp -d | str trim
}

# ─── Test cases ───────────────────────────────────────────────────────────────

def test-default-audit-only [nono: string, root: string] {
  step "Case 1: default — audit-only session created"
  let workdir = test-dir
  let before = sessions-snapshot $root

  ^$nono run --allow-cwd --allow $workdir -- echo "audit-only-test" o+e> /dev/null

  let new = new-sessions $root $before
  assert (($new | length) == 1) "expected exactly 1 new session dir"

  let session = $new | first
  assert (has-session-json $session) "session.json should exist"
  assert (not (has-snapshots $session)) "should have no snapshots (audit-only)"

  rm -rf $workdir
  print $"    (ansi green)PASS(ansi reset) — session.json created, no snapshots"
}

def test-no-audit [nono: string, root: string] {
  step "Case 2: --no-audit — nothing persisted"
  let workdir = test-dir
  let before = sessions-snapshot $root

  ^$nono run --allow-cwd --allow $workdir --no-audit -- echo "no-audit-test" o+e> /dev/null

  let new = new-sessions $root $before
  assert (($new | length) == 0) "expected no new session dirs"

  rm -rf $workdir
  print $"    (ansi green)PASS(ansi reset) — no session created"
}

def test-rollback-with-audit [nono: string, root: string] {
  step "Case 3: --rollback — audit + snapshots"
  let workdir = test-dir
  let before = sessions-snapshot $root

  ^$nono run --allow-cwd --allow $workdir --rollback -- echo "rollback-test" o+e> /dev/null

  let new = new-sessions $root $before
  assert (($new | length) == 1) "expected exactly 1 new session dir"

  let session = $new | first
  assert (has-session-json $session) "session.json should exist"
  assert (has-snapshots $session) "should have snapshots"

  rm -rf $workdir
  print $"    (ansi green)PASS(ansi reset) — session.json + snapshots created"
}

def test-rollback-no-audit [nono: string, root: string] {
  step "Case 4: --rollback --no-audit — snapshots only"
  let workdir = test-dir
  let before = sessions-snapshot $root

  ^$nono run --allow-cwd --allow $workdir --rollback --no-audit -- echo "rollback-no-audit-test" o+e> /dev/null

  let new = new-sessions $root $before
  assert (($new | length) == 1) "expected exactly 1 new session dir"

  let session = $new | first
  assert (has-session-json $session) "session.json should exist (rollback writes it)"
  assert (has-snapshots $session) "should have snapshots"

  rm -rf $workdir
  print $"    (ansi green)PASS(ansi reset) — snapshots created, audit skipped"
}

# ─── Entry point ──────────────────────────────────────────────────────────────

def main [] {
  let release = $"($env.HOME)/.cargo-target/nono/release/nono"
  let debug = $"($env.HOME)/.cargo-target/nono/debug/nono"
  let nono = if ($release | path exists) { $release } else { $debug }
  let root = $"($env.HOME)/.nono/rollbacks"

  assert ($nono | path exists) $"nono binary not found at ($nono) — run 'make build' first"

  print $"Testing audit/rollback decoupling with: ($nono)"
  print ""

  test-default-audit-only $nono $root
  test-no-audit $nono $root
  test-rollback-with-audit $nono $root
  test-rollback-no-audit $nono $root

  print ""
  step "All 4 cases passed"
}
