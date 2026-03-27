# bentos-execd Build State — john-execd-01 nap

## Completed: M0-M7 (full exec stack)

### bentos-execd (Rust guest agent)
- macOS: 26 tests, Linux (Docker): 46 tests. All green, zero warnings.
- 528 KB static ARM64 ELF, stripped, statically linked.
- Module layout: main.rs / proto.rs / wire.rs / exec.rs / session.rs / listener.rs

### M6 — Deployment (bentos_distro) — commit a5956b7
- `lib/bentos_distro/configs/etc/init.d/bentos-execd` — OpenRC service
  - `/usr/sbin/bentos-execd`, vsock default, background + pidfile
  - `before bentosd`, `before networking`, `after localmount`
- `lib/bentos_distro/scripts/build-rootfs.sh` — Stage 1.5: host-side Rust cross-compile
  - Binary → `/usr/sbin/bentos-execd` in rootfs
  - Service → `/etc/init.d/bentos-execd`
  - `rc-update add bentos-execd default` (before bentosd)

### M7 — Swift VMM Exec Endpoints (bentos_vmm_macos) — commit e41e8a6
- `GET /api/v1/machines/{id}/exec` — unified WebSocket exec (interactive + one-shot via client `.collect()`)
- New files: ExecHandler.swift (WS↔vsock byte pipe), ExecSession.swift (TLV + hand-encoded protobuf)
- Modified: Router.swift, Types.swift, MachineManager.swift (vsockConnect), HttpHandler.swift, HttpServer.swift
- 125 Swift tests passing (+4 new router tests)

> **S314 API change:** `POST /exec/run` removed. One-shot is now client-side: open WS, send STDIN_EOF, collect frames. No separate `execRun()` Dart method — use `exec(...).collect()`. See `hq/workshop/notes/s314-exec-api-design.md`.

### Report
- `hq/c-wing/cafe/john-report.md` Section 7 written — commit 4f7f5a2

### Full stack
```
host CLI → WS /exec
  → bentos-vmm-macos
  → VZVirtioSocketDevice.connect(toPort: 5100)
  → vsock
  → bentos-execd (Alpine, /usr/sbin/, OpenRC default runlevel)
  → fork/exec guest process
  → TLV stdout/stderr/ExitStatus back
```

### Infrastructure
- Rust 1.94.1, aarch64-unknown-linux-musl, musl-cross, protoc, Docker/OrbStack
- Swift 6.1, swift-nio 2.65+, Virtualization.framework macOS 14+
