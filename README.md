# bentos-execd

[![CI](https://github.com/cafe01/bentos-execd/actions/workflows/ci.yml/badge.svg)](https://github.com/cafe01/bentos-execd/actions/workflows/ci.yml)

Guest agent for structured command execution inside BentOS VMs. The `docker exec` equivalent for BentOS.

A standalone static binary that runs inside the guest, listens on AF_VSOCK port 5100, and executes commands on behalf of the host VMM daemon. Written in Rust, cross-compiled to `aarch64-unknown-linux-musl`.

## Why separate from bentosd

**You need exec when bentosd is broken.** The primary use case for `bentos vm shell` is debugging a misbehaving guest. If exec is embedded in bentosd, the tool you need for diagnosis dies with the patient.

**Different domain.** bentosd manages devices and drivers (CUSE/FUSE, container orchestration). bentos-execd manages process lifecycle (fork/exec, PTY, I/O streaming). Different concerns, different failure modes.

**Boot order.** Starts before networking, before bentosd — the earliest possible moment the guest is responsive to the host.

**Small scope.** ~500 lines of Rust. Listen, fork, exec, stream I/O, exit.

## Architecture

```
Host (macOS)                           Guest (Alpine ARM64)
─────────────────                      ─────────────────────
bentos-vmm-macos                       bentos-execd
  │                                      │
  │ GET /exec (WebSocket)                │ listen AF_VSOCK :5100
  │  (interactive + one-shot)            │
  │                                      │
  │  VZVirtioSocketDevice                │
  │    .connect(toPort: 5100)            │
  │         │                            │
  └─────────┼── vsock SOCK_STREAM ──────►│
            │                            │
            │   TLV-framed protobuf      │
            │   ◄──────────────────►     │
            │                            │
            │                            ├─ fork()/exec() [non-TTY]
            │                            │  or forkpty()  [TTY]
            │                            │
            │                            ├─ stream stdout/stderr
            │                            ├─ forward stdin
            │                            └─ send exit code
```

One connection = one process. No multiplexing. Multiple concurrent execs = multiple vsock connections.

## Wire Protocol

TLV framing over SOCK_STREAM vsock. Defined in `exec_wire.proto`.

### Frame Format

```
+--------+--------+------------------+
| type   | length | payload          |
| 1 byte | 4 byte | <length> bytes   |
| u8     | u32 LE |                  |
+--------+--------+------------------+
```

5-byte header + payload. Maximum payload: 1 MB.

### Message Types

**Host -> Guest (0x01-0x0F):**

| Type | Value | Payload | Description |
|------|-------|---------|-------------|
| EXEC_REQUEST | 0x01 | protobuf `ExecRequest` | Start a process |
| STDIN_DATA | 0x02 | Raw bytes | Data for process stdin |
| STDIN_EOF | 0x03 | Empty | Close process stdin |
| WINDOW_RESIZE | 0x04 | protobuf `WindowResize` | Terminal resize (tty mode) |
| SIGNAL | 0x05 | protobuf `Signal` | Forward signal to process |

**Guest -> Host (0x10-0x1F):**

| Type | Value | Payload | Description |
|------|-------|---------|-------------|
| EXEC_STARTED | 0x10 | protobuf `ExecStarted` | Process started, here's the PID |
| STDOUT_DATA | 0x11 | Raw bytes | Process stdout |
| STDERR_DATA | 0x12 | Raw bytes | Process stderr |
| EXIT_STATUS | 0x13 | protobuf `ExitStatus` | Process exited |
| EXEC_ERROR | 0x14 | protobuf `ExecError` | Failed to start process |

Raw byte payloads (STDIN_DATA, STDOUT_DATA, STDERR_DATA) are NOT protobuf-encoded — they are raw bytes directly in the TLV payload. This avoids double-copying I/O data through protobuf serialization.

### Protocol Sequence

```
Host                          Guest
  │                              │
  │── EXEC_REQUEST ────────────►│   fork/exec (or forkpty)
  │                              │
  │◄──────────── EXEC_STARTED ──│   pid assigned
  │                              │
  │── STDIN_DATA ──────────────►│   -> child stdin
  │── STDIN_DATA ──────────────►│
  │── STDIN_EOF ───────────────►│   close child stdin
  │                              │
  │◄──────────── STDOUT_DATA ──│   child stdout
  │◄──────────── STDERR_DATA ──│   child stderr (non-tty only)
  │◄──────────── STDOUT_DATA ──│
  │                              │
  │── WINDOW_RESIZE ──────────►│   ioctl(TIOCSWINSZ) (tty only)
  │── SIGNAL ─────────────────►│   kill(pid, sig)
  │                              │
  │◄──────────── EXIT_STATUS ──│   waitpid() result
  │                              │
  [connection closed]             [connection closed]
```

## Execution Modes

### Non-TTY (`tty: false`)

`pipe()` + `fork()`/`exec()`. Stdout and stderr are separate streams (STDOUT_DATA vs STDERR_DATA). No terminal processing. Signals forwarded via explicit SIGNAL message. Used for scripted/CI execution.

### TTY (`tty: true`)

`forkpty()` allocates a pseudo-terminal. Stdout and stderr are merged (both come as STDOUT_DATA — that's how PTYs work). WINDOW_RESIZE triggers `ioctl(master_fd, TIOCSWINSZ, &ws)` which delivers SIGWINCH. Ctrl-C (0x03) is handled natively by the PTY terminal driver. Used for interactive shells and TUI applications.

## Deployment

- **Binary**: `/usr/sbin/bentos-execd` — static musl binary, < 1 MB
- **Service**: OpenRC init script, starts before networking, before bentosd
- **Port**: AF_VSOCK 5100 (above the 5003-5099 driver range)
- **Target**: `aarch64-unknown-linux-musl` (cross-compiled from macOS host)

## Host-Side Integration

The VMM daemon (bentos-vmm-macos, Swift) bridges between HTTP/WebSocket clients and vsock:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/machines/{id}/exec` | GET | WebSocket upgrade. Streaming TLV frames. Handles both interactive and one-shot exec (one-shot is a client-side pattern via `.collect()`). |

The daemon connects to guest vsock port 5100 via `VZVirtioSocketDevice.connect(toPort: 5100)`, translates between HTTP/WebSocket and TLV vsock frames.

## Port Allocation (from L08)

| Port | Purpose |
|------|---------|
| 5000 | Control plane (bentosd) |
| 5001 | Log streaming |
| 5002 | Metrics |
| 5003-5099 | Driver data channels |
| **5100** | **Exec agent (bentos-execd)** |

## Stack Precedent

bentos_fuse uses the same pattern: protobuf over length-prefixed framing (4-byte big-endian length + protobuf payload). bentos-execd uses TLV (1-byte type + 4-byte LE length + payload) because exec messages need a type discriminator in the header — the type byte determines whether the payload is protobuf or raw bytes.

## References

- `university/cs/apple-virtualization/lessons/13-vm-exec.md` — Full architecture lesson
- `lib/bentos_fuse/README.md` — Protobuf framing precedent in the stack
- `lib/bentos_vmm_macos/TACTICAL_PLAN.md` — VMM daemon plan (M7 exec integration)
