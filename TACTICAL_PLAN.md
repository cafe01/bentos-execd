# bentos-execd: Tactical Plan

> Implementor: John (SWE)
> Owner: Cafe (CTO) + Alfred (CPO/COO)
> Status: M0-M5.4 complete (Docker validated), M5.5 optional, M6 deployment next

One deliverable: a static Rust binary (`bentos-execd`) that runs inside BentOS guest VMs, listens on AF_VSOCK port 5100, and executes commands on behalf of the host. The `docker exec` equivalent for BentOS.

---

## Design Principles

1. **Independence.** bentos-execd has zero dependencies on bentos-vmm-macos, bentosd, or any other BentOS component for building, testing, or validating M0-M5. It is a self-contained Rust crate.

2. **Listener abstraction.** The socket layer is trait-based (`Listener` + `Connection`). The full integration path — listener → accept → TLV framing → session dispatch → exec — is testable with TCP on macOS and Linux. Only the thin `VsockListener` adapter is `#[cfg(target_os = "linux")]`.

3. **Self-contained testing.** M0-M2: macOS native. M3-M5: Docker containers (`arm64` or `amd64` Alpine Linux) — no VM, no external infra. The only tests that require a running BentOS VM are the deployment smoke test in M6 and the host-side integration in M7.

4. **Platform gating is surgical.** Only code that *calls* Linux-specific syscalls (fork/exec/forkpty/AF_VSOCK) is `#[cfg(target_os = "linux")]`. Session logic, framing, and the listener accept loop are platform-agnostic.

---

## Operating Model

Sequential milestones. M0-M5 are guest-side Rust work. M6 is deployment packaging. M7 is host-side Swift work on bentos-vmm-macos (separate crate, separate plan).

| Milestone | Test environment | What to test |
|-----------|-----------------|-------------|
| M0 | macOS | Cross-compilation succeeds, binary is static musl |
| M1 | macOS | Protobuf encode/decode round-trips |
| M2 | macOS | TLV frame read/write, boundary conditions, max payload |
| M3 | Docker (Linux) | Non-TTY exec: pipe+fork+exec, stdout/stderr, exit codes, signals |
| M4 | Docker (Linux) | TTY exec: forkpty, merged output, TIOCSWINSZ, PTY signals |
| M5 | macOS + Docker | Listener abstraction tests (macOS), full integration tests (Docker) |
| M6 | Docker + VM | OpenRC service, rootfs integration, deployment smoke test |
| M7 | macOS | Swift VMM endpoints — separate crate, separate plan |

### Docker Testing Strategy

For M3-M5 Linux tests:

```sh
# Build the test binary for Linux musl
cargo build --tests --target aarch64-unknown-linux-musl --release

# Run tests in a minimal Alpine container
docker run --rm -v ./target/aarch64-unknown-linux-musl/release/deps:/tests alpine:3.19 \
  sh -c 'for t in /tests/bentos_execd-*; do [ -x "$t" ] && "$t" --test-threads=1; done'
```

Alternative for x86_64 hosts: use `x86_64-unknown-linux-musl` target and `alpine:3.19` (amd64). The exec engine code is architecture-independent.

CI can run the Docker tests automatically. No VM required.

### TDD Discipline

Every subtask has test criteria. Rust tests (`cargo test`) are the primary feedback loop.

- **M0-M2**: `cargo test` on macOS — all tests run natively.
- **M3-M4**: `cargo test` on macOS compiles but skips Linux-guarded tests. Full validation via Docker.
- **M5**: Listener/accept/framing tests run on macOS (TCP backend). Full exec integration tests via Docker.

### Handoff Protocol

Each `.N` subtask boundary is a valid nap point. When handing off:
1. State what subtask completed (with test count).
2. State what's next (the next `.N`).
3. Note any deviation from plan or gotcha discovered.

The successor runs `cargo test` to verify green, then continues.

---

## Milestones

### M0: Rust Project Skeleton [COMPLETE]

Cargo project compiles and cross-compiles to `aarch64-unknown-linux-musl`. No functionality — just the build infrastructure.

- [x] **M0.1** Cargo.toml + src/main.rs
- [x] **M0.2** Cross-compilation setup
- [x] **M0.3** Project structure

**Milestone validation**: `cargo build --target aarch64-unknown-linux-musl --release` produces a static ARM64 Linux binary under 1 MB. ✓ (900 KB)

### M1: Protobuf Schema + Codegen [COMPLETE]

Wire protocol types generated from `exec_wire.proto` via prost.

- [x] **M1.1** build.rs with prost codegen
- [x] **M1.2** Message type constants
- [x] **M1.3** Protobuf round-trip tests (8 tests)

**Milestone validation**: All protobuf types compile, encode, and decode correctly. Type constants defined. ✓

### M2: TLV Framing Layer [COMPLETE]

Read and write TLV frames over any `Read`/`Write` stream. Transport-agnostic.

- [x] **M2.1** `write_frame` (3 tests)
- [x] **M2.2** `read_frame` (5 tests)
- [x] **M2.3** Helper functions for protobuf messages (3 tests)
- [x] **M2.4** Bidirectional frame test over TCP (1 test)

**Milestone validation**: `cargo test` — all framing tests pass. ✓ (23 tests total through M2)

### M3: Exec Engine — Non-TTY Mode [COMPLETE]

Process execution with pipe-based I/O. All tests validated in Docker.

- [x] **M3.1** `exec_non_tty(req: ExecRequest) -> Result<Child>` (3 tests)
- [x] **M3.2** Stdin pipe (2 tests)
- [x] **M3.3** Signal forwarding (2 tests)
- [x] **M3.4** Wait and exit status (3 tests)
- [x] **M3.5** Non-TTY I/O loop — session_non_tty over TCP (2 tests)
- [x] **M3.6** Docker test validation — all 10 M3 tests green in Alpine ARM64 container

**Milestone validation**: `cargo test` in Docker — all non-TTY exec tests pass. ✓

### M4: TTY Mode [COMPLETE]

Interactive execution with pseudo-terminal. All tests validated in Docker.

- [x] **M4.1** `exec_tty(req: ExecRequest) -> Result<TtyChild>` (in session.rs)
- [x] **M4.2** Window resize — `resize_pty`
- [x] **M4.3** TTY I/O loop — `session_tty` over TCP (1 test)
- [x] **M4.4** Unified session dispatcher — `handle_connection` (3 tests)
- [x] **M4.5** Docker test validation — all M4 tests green in Alpine ARM64 container

**Milestone validation**: `cargo test` in Docker — all TTY tests pass. ✓

### M5: Listener Abstraction + Full Integration [M5.1-M5.4 COMPLETE]

The listener layer is trait-based. TCP backend works on all platforms. vsock backend is Linux-only. The full integration path (listener → accept → handle_connection → exec) is testable via TCP.

**Already done:**
- [x] `Connection` trait + `Listener` trait (platform-agnostic)
- [x] `TcpListener` backend (always available)
- [x] `VsockListener` + `VsockStream` backend (`#[cfg(target_os = "linux")]`)
- [x] `serve()` accept loop (platform-agnostic)
- [x] TCP listener accept test (macOS, 1 test)
- [x] Full integration tests through TCP (Linux-guarded, 3 tests)

**Remaining:**

- [x] **M5.1** Make integration tests platform-agnostic where possible
  - `handle_connection` made platform-agnostic (returns EXEC_ERROR on non-Linux)
  - `serve()` made platform-agnostic
  - Added 2 cross-platform tests: `tcp_listener_handle_connection_cross_platform` + `tcp_listener_invalid_request_cross_platform`
  - macOS: 26 tests (24 original + 2 new). Docker: 45 tests (43 original + 2 new).

- [x] **M5.2** main() wiring verified
  - `--port PORT`, `--log-level LEVEL`, `--help`, `--version` all working
  - SIGTERM handler on Linux, env_logger initialized
  - `--help` prints usage; `--version` prints version ✓

- [x] **M5.3** main() TCP mode for development
  - Added `--tcp ADDR` flag — uses TCP listener instead of vsock
  - On Linux: `--tcp` for dev/Docker, vsock by default
  - On macOS: `--tcp` required (vsock unavailable), exits with helpful message otherwise
  - Docker validation: `bentos-execd --tcp 0.0.0.0:5100` starts, listens, logs correctly ✓

- [x] **M5.4** Docker full integration test
  - `tcp_serve_end_to_end_integration` test: serve() loop → accept → non-TTY exec + TTY session + invalid request
  - Both non-TTY and TTY modes verified through the full stack over TCP
  - Binary validated in Docker with `--tcp 0.0.0.0:5100` — starts, listens, logs correctly
  - **Test**: 1 new comprehensive e2e test, 46 total Docker tests green ✓

- [ ] **M5.5** vsock integration test (Linux only, optional)
  - If a vsock-capable environment is available (EC2, BentOS VM): test actual vsock path
  - This validates the thin VsockListener/VsockStream adapter
  - Not a blocker — the TCP integration test covers 99% of the code path
  - **Test**: manual or CI test on vsock-capable host

**Milestone validation**: `cargo test` on macOS passes all platform-agnostic tests. Docker integration test exercises the full listener → accept → exec → TLV response path over TCP. The vsock adapter is a thin wrapper over the same code.

### M6: Deployment

Package bentos-execd into the BentOS guest image with proper service management.

- [ ] **M6.1** OpenRC service file
  - `/etc/init.d/bentos-execd`:
    ```sh
    #!/sbin/openrc-run
    name="bentos-execd"
    description="BentOS guest exec agent"
    command="/usr/sbin/bentos-execd"
    command_background=true
    pidfile="/run/bentos-execd.pid"
    depend() {
        before bentosd
        before networking
        after localmount
    }
    ```
  - Starts before networking and before bentosd
  - After localmount (needs /usr/sbin accessible)
  - **Test**: Docker Alpine container with OpenRC — `rc-service bentos-execd start` succeeds

- [ ] **M6.2** Distro integration
  - Add bentos-execd to `lib/bentos_distro/` rootfs build (install binary to `/usr/sbin/`)
  - Add service file to rootfs
  - Enable service in default runlevel: `rc-update add bentos-execd default`
  - **Test**: boot fresh VM from built image; `rc-status` shows bentos-execd running; vsock port 5100 is listening

- [ ] **M6.3** Binary size verification
  - `cargo build --target aarch64-unknown-linux-musl --release`
  - Verify: `file` shows static, `ls -la` shows < 1 MB
  - If over 1 MB: audit dependencies, consider `cargo-bloat`, remove unnecessary features
  - **Test**: binary is statically linked, < 1 MB, runs on Alpine ARM64

**Milestone validation**: Boot a BentOS VM from a fresh image. `bentos-execd` is running automatically. Exec from host works without manual intervention.

### M7: VMM Exec Endpoints (bentos-vmm-macos — Swift)

**Out of scope for bentos-execd.** This is host-side work on bentos-vmm-macos. Documented here for reference but lives on the VMM tactical plan.

- [ ] **M7.1** vsock exec connection (`VZVirtioSocketDevice.connect(toPort: 5100)`)
- [ ] **M7.2** Unified WebSocket exec endpoint (`GET /api/v1/machines/{id}/exec`) — handles both interactive and one-shot (S314 unified API)
- [ ] **M7.3** Router integration

**Milestone validation**: From host CLI: one-shot exec and interactive WebSocket sessions work through the VMM daemon.

---

## Cross-Compilation Gotchas

Things John will hit:

1. **musl-cross on macOS.** `brew install filosottile/musl-cross/musl-cross` provides the `aarch64-linux-musl-gcc` linker. Without it, `cargo build --target aarch64-unknown-linux-musl` fails at link time.

2. **Static linking verification.** `file target/.../bentos-execd` must show "statically linked". If it shows "dynamically linked", a dependency is pulling in a system library. Check `ldd` output (on a Linux host) or `readelf -d`.

3. **protobuf codegen in cross-compile.** `prost-build` runs `protoc` at build time on the HOST (macOS), generating Rust code that then cross-compiles. This works because protobuf codegen is target-independent. But `protoc` must be installed on macOS: `brew install protobuf`.

4. **nix crate feature flags.** Only enable needed features to minimize binary size: `process`, `signal`, `term`, `ioctl`, `poll`, `fs`. Don't enable `net`, etc.

5. **AF_VSOCK not available on macOS.** The vsock listener code compiles for Linux only. Use `#[cfg(target_os = "linux")]` guards. Only the VsockListener/VsockStream types need this guard — the rest of the stack (listener trait, session, framing) is platform-agnostic.

6. **Binary size.** If over 1 MB after strip+LTO+opt-level=z, the usual suspects: `tokio` (pulls in a lot), `serde` (derive macros inflate), `regex` (Unicode tables). Keep deps minimal. `prost` + `nix` should be fine.

7. **Cross-compiling tests.** `cargo build --tests --target aarch64-unknown-linux-musl` builds test binaries. Run them in Docker. If cross-compiling tests is cumbersome, use `x86_64-unknown-linux-musl` as an alternative test target.

---

## Dependencies

| Crate | Version | Purpose | Binary size impact |
|-------|---------|---------|-------------------|
| `prost` | 0.13+ | Protobuf encode/decode | ~100 KB |
| `prost-types` | 0.13+ | Well-known protobuf types | Minimal |
| `nix` | 0.29+ | POSIX syscalls (fork, exec, pty, signals, ioctl, sockets) | ~50 KB |
| `log` | 0.4 | Logging facade | ~5 KB |
| `env_logger` | 0.11 | Logging implementation | ~30 KB |
| **Build** | | | |
| `prost-build` | 0.13+ | Protobuf codegen (host only) | 0 (build-time) |

**Total estimated**: ~200-400 KB stripped static binary. Well under 1 MB target.

Optional (John decides):
- `tokio` — if async vsock listener is preferred over std threads. Adds ~300-500 KB.

---

## What's Explicitly Deferred

- User/group switching (`--user` flag for exec) — add when multi-user support needed
- Resource limits (cgroups, rlimits) on exec'd processes
- Exec audit logging (who ran what when)
- Connection authentication (vsock is host-only, but future remote exec may need it)
- Multiplexing multiple processes on one connection
- Dart CLI integration (`bentos vm exec`, `bentos vm shell`) — separate deliverable
- Console (Flutter) integration for exec
