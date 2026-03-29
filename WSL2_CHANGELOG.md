# WSL2 Support — Implementation Record

Tracks progress against [WSL2_SUPPORT_PLAN.md](WSL2_SUPPORT_PLAN.md).

## Environment

- **WSL2 distro**: Ubuntu 20.04
- **WSL2 kernel**: 6.6.87.2-microsoft-standard-WSL2
- **Landlock ABI**: V3 (filesystem only, no TCP filtering)
- **GCC**: 10 (upgraded from 9 to fix aws-lc-sys memcmp bug)
- **Branch**: `feat/wsl2-support`

---

## Track 1.1 — WSL2 Detection ✅

**Commit**: `4b5fc88` feat(wsl2): add WSL2 detection, feature matrix, and integration tests

### What was done

**Library (`crates/nono/src/sandbox/linux.rs`)**:
- Added `is_wsl2()` — cached detection via `OnceLock`
  - Checks `/proc/sys/fs/binfmt_misc/WSLInterop` (filesystem indicator)
  - Checks `WSL_DISTRO_NAME` env var (set by WSL init)
  - Result cached for process lifetime
- Added `Wsl2FeatureMatrix` struct with `detect()` and `summary()` methods
  - Reports availability of: filesystem sandbox, block-all network, per-port network, supervisor mode, proxy filter, capability expansion
- Re-exported through `sandbox/mod.rs` and `lib.rs`

**Unit tests (7)** in `linux.rs`:
| Test | Validates |
|------|-----------|
| `test_is_wsl2_does_not_panic` | No crash in any environment |
| `test_is_wsl2_consistent` | OnceLock returns stable results |
| `test_detect_wsl2_matches_indicators` | Agrees with raw filesystem/env checks |
| `test_wsl2_feature_matrix_detect` | Feature flags correct per environment |
| `test_wsl2_feature_matrix_summary_not_empty` | Summary text is meaningful |
| `test_wsl2_feature_matrix_filesystem_matches_landlock` | Matches `is_supported()` |
| `test_wsl2_per_port_matches_abi_v4` | Matches Landlock V4 probe |

**Integration tests (18)** in `tests/integration/test_wsl2.sh`:
| Section | Tests | Plan track |
|---------|-------|------------|
| WSL2 Detection | Setup reports WSL2, indicators present | 1.1 |
| Filesystem Sandbox | allow, write, read, deny outside path | 1.2 |
| Block-All Network | curl blocked, netcat blocked | 1.3 |
| Per-Port Filtering | Correctly rejected (no V4) | 1.3 |
| Supervised Mode | Default exec works on WSL2 | 1.2/1.4 |
| Setup Reporting | Exits 0, platform/Landlock/kernel info | 1.4 |
| Direct Mode | wrap runs, output correct, exit codes | 1.2 |

**Test helpers** (`tests/lib/test_helpers.sh`):
- `is_wsl2()` — shell-level WSL2 detection
- `skip_unless_wsl2()` — skip test unless on WSL2
- `skip_on_wsl2()` — skip test if on WSL2

**Dev tooling**:
- `scripts/wsl-dev.sh` — sync/build/test across Windows↔WSL2
  - Handles: setup, sync (git + uncommitted files via rsync), build, test, test-wsl2, ci, shell
  - Auto-installs: Rust, build-essential, pkg-config, libdbus-1-dev, gcc-10 (if needed)
  - Fixes CRLF line endings on shell scripts after sync
- `scripts/run-all-tests.sh` — runs all 22 integration test suites with summary

### What we learned

1. **Basic supervised mode works on WSL2** — the `EBUSY` from `SECCOMP_RET_USER_NOTIF` only triggers when capability elevation or proxy filtering is active, not on the default supervised exec path
2. **Filesystem sandboxing is fully functional** — Landlock V1-V3 works identically to native Linux
3. **Block-all network works** — `SECCOMP_RET_ERRNO` has no conflict with WSL2's seccomp filter
4. **Per-port filtering correctly rejected** — Landlock V4 needs kernel 6.7+, WSL2 is on 6.6
5. **Existing test suite mostly passes** — 244/254 tests pass, 10 failures are all pre-existing (missing D-Bus keyring, macOS-only assertions, test fixture bugs), none WSL2-specific

---

## Track 1.1 (supplementary) — .gitattributes ✅

**Commit**: `736aca3` chore: add .gitattributes to enforce LF line endings for scripts

Windows Git converts LF→CRLF on checkout, which breaks shell scripts in WSL2 (`$'\r': command not found`). Added `.gitattributes` to force LF on `*.sh`, `*.rs`, `*.toml` files.

---

## Track 1.2 — Skip seccomp notify on WSL2 ✅

**Goal**: Guard `install_seccomp_notify()` and `install_seccomp_proxy_filter()` calls with WSL2 check. When capability elevation or proxy filtering is requested on WSL2, warn and fall back instead of hitting `EBUSY`.

### What was done

**3 guard points added:**

1. **`main.rs`** — capability elevation guard: if `--capability-elevation` is set on WSL2, force it to false and print warning. Prevents seccomp notify from being attempted.

2. **`main.rs`** — proxy fallback guard: if `ProxyOnly` network mode would need seccomp proxy filter on WSL2, force `seccomp_proxy_fallback` to false and print warning. Falls back to block-all network.

3. **`exec_strategy.rs`** — defense-in-depth guards on both `install_seccomp_notify()` and `install_seccomp_proxy_filter()` call sites in the child process post-fork. These should never be reached due to the main.rs guards, but protect against configs constructed without going through main.rs.

**User-visible behavior:**

```
$ nono run --capability-elevation --allow /tmp -- echo "hello"
  [nono] WSL2 detected: capability elevation disabled (seccomp user notification unavailable, see microsoft/WSL#9548)
  Applying sandbox... active
  hello
```

Command runs successfully with static Landlock capabilities instead of crashing with `EBUSY`.

### Why seccomp notify fails on WSL2

WSL2's init process (PID 1) installs its own `SECCOMP_RET_USER_NOTIF` filter for Windows interop (running `.exe` files from Linux). The Linux kernel only allows **one** user notification listener per filter chain. When nono tries to install a second one, it gets `EBUSY`. See [microsoft/WSL#9548](https://github.com/microsoft/WSL/issues/9548) (open since Jan 2023).

### Landlock ABI versions on WSL2

WSL2 kernel 6.6 supports Landlock V3. The ABI version determines available features:

| Landlock ABI | Kernel | Feature | WSL2 6.6 |
|-------------|--------|---------|----------|
| V1 | 5.13+ | Basic filesystem | ✅ |
| V2 | 5.19+ | File rename (Refer) | ✅ |
| V3 | 6.2+ | File truncation | ✅ |
| V4 | 6.7+ | TCP network filtering | ❌ |
| V5 | 6.10+ | Device ioctl filtering | ❌ |
| V6 | 6.12+ | Process scoping | ❌ |

Per-port network filtering (`--allow-net 443`) requires V4. This will become available automatically when Microsoft upgrades the WSL2 kernel to 6.7+ — no nono code changes needed since `detect_abi()` already probes for the highest available version.

---

## Track 1.3 — Network strategy on WSL2 ✅

**Goal**: Ensure all network modes work or degrade gracefully on WSL2.

### What was done

After investigating the proxy architecture, we found the original plan's "out-of-process proxy" approach was unnecessary. The credential proxy **already works on WSL2** because it runs in the unsandboxed parent process:

1. Parent starts proxy on `127.0.0.1:{random_port}` (before fork)
2. Loads credentials from system keystore
3. Sets env vars (`HTTP_PROXY`, `OPENAI_API_KEY`, etc.)
4. Forks child, applies Landlock, execs command
5. Child routes API calls through proxy automatically via env vars
6. Proxy injects credentials and forwards to upstream

**What's degraded**: Network port-level enforcement. On native Linux with Landlock V4+, the child is restricted to *only* connect to `127.0.0.1:{proxy_port}`. On WSL2 with V3, there's no port filtering — the child *could* bypass the proxy and connect directly. The seccomp fallback that would catch this is also unavailable (EBUSY).

**Why we didn't implement Unix socket proxy or other workarounds:**
- Unix socket proxy: HTTP clients need TCP to speak HTTP proxy protocol (`CONNECT` tunnels). Blocking all TCP via seccomp then using a Unix socket creates a chicken-and-egg problem — standard SDKs (Python `requests`, Node `fetch`, Go `net/http`) don't support Unix socket proxies.
- Network namespaces: Requires root (CAP_NET_ADMIN).
- eBPF cgroup socket filter: Requires root for cgroup setup.
- iptables owner match: Requires root.

**Why this is acceptable**: The credential proxy is still valuable even without port enforcement — it keeps secrets out of the child's env vars, provides audit logging, and enables L7 endpoint filtering. The gap is only that a malicious child could bypass the proxy to make direct connections, but the proxy itself works correctly.

**Future fix**: When Microsoft upgrades the WSL2 kernel to 6.7+ (Landlock V4), port-level lockdown activates automatically. `detect_abi()` already probes for V4, and `NetworkMode::ProxyOnly` already applies `NetPort` rules when V4 is available. **Zero code changes needed.**

Updated the seccomp proxy fallback warning in `main.rs` to accurately describe the situation:
```
[nono] WSL2 detected: seccomp proxy network enforcement disabled
       (seccomp user notification unavailable, see microsoft/WSL#9548).
       Credential proxy still active but port-level lockdown unavailable
       until Landlock V4 (kernel 6.7+).
```

### Network mode summary on WSL2

| Mode | Works | Enforcement |
|------|-------|-------------|
| `--block-net` | ✅ | Kernel-enforced (seccomp `RET_ERRNO`) |
| `--allow-net 443` | ❌ Rejected | Needs Landlock V4 (kernel 6.7+) |
| `--credential openai` | ✅ Functional | Proxy works, but child not port-locked to proxy |
| Default (allow all) | ✅ | No restriction |

---

## Track 1.4 — CLI UX ✅

**Goal**: Clear warnings and error messages for WSL2 limitations.

### What was done

**`setup --check-only` WSL2 feature matrix** (`setup.rs`):

When WSL2 is detected, `nono setup --check-only` now prints a full feature availability matrix:

```
  * WSL2 environment detected
    - Filesystem sandbox: available (Landlock V3)
    - Block-all network (--block-net): available
    - Per-port network filtering: unavailable (needs kernel 6.7+ for Landlock V4)
    - Credential proxy (--credential): functional (port enforcement degraded)
    - Supervisor mode: available (basic)
    - Capability elevation (--capability-elevation): unavailable
      seccomp user notification returns EBUSY (microsoft/WSL#9548)
```

**Runtime warnings** (implemented in Track 1.2):
- `--capability-elevation` on WSL2: `[nono] WSL2 detected: capability elevation disabled ...`
- Proxy with seccomp fallback on WSL2: `[nono] WSL2 detected: seccomp proxy network enforcement disabled ...`

**Per-port filtering rejection**: Already handled by Landlock — attempting V4 network rules on a V3 kernel returns an error naturally. No WSL2-specific code needed.

---

## Track 1.5 — Documentation ✅

**Goal**: Compatibility matrix, seccomp limitation docs, workarounds.

### What was done

Added `docs/cli/internals/wsl2.mdx` — comprehensive WSL2 documentation covering:

- Quick summary of what works and what's limited
- Full compatibility matrix (14 features)
- WSL2 detection mechanism
- Landlock ABI version table with kernel version mapping
- seccomp user notification limitation explained (what it is, why it fails, what nono does)
- Credential proxy behavior on WSL2
- Workarounds: custom kernel build instructions, block-all network
- Future improvements: Landlock V4, eBPF LSM, microsoft/WSL#9548

Added link to WSL2 page in `docs/cli/internals/index.mdx`.

---

## Track 2.1 — Landlock V4 (kernel upgrade) 🔲

**No code changes needed.** ABI auto-detection already handles V4 when available. Depends on Microsoft upgrading WSL2 kernel from 6.6 to 6.7+.

---

## Track 2.2 — Supervisor alternatives 🔲

**Goal**: Evaluate eBPF LSM vs ptrace as alternatives to seccomp notify on WSL2.

**Status**: Not started. eBPF LSM is most promising — WSL2 kernel has `CONFIG_BPF_LSM=y` enabled.

| Approach | Kernel-enforced | Conflict with WSL2 | Effort |
|----------|----------------|-------------------|--------|
| eBPF LSM | Yes | No | High |
| ptrace | Yes | No | Medium |
| LD_PRELOAD | No (bypassable) | No | Low |
| Wait for MS fix | N/A | N/A | None |

---

## Pre-existing test failures (not WSL2-related)

These failures exist on native Linux too:

| Suite | Failure | Cause |
|-------|---------|-------|
| `test_edge_cases.sh` (2) | relative path, `..` path | Binary path is relative, breaks when test cds |
| `test_learn.sh` (1) | learn traces cat | strace output format difference |
| `test_override_deny.sh` (1) | child profile inherits override_deny | Missing test fixture profile |
| `test_silent_output.sh` (1) | macOS keychain warning | macOS-only assertion on Linux |
| `test_trust_cli.sh` (5) | keygen, init, sign-policy | Missing `org.freedesktop.secrets` D-Bus service |
| `test_client_startup.sh` | npm install timeout | Infra/timing issue |
