# nix 0.29 API Gotchas (aarch64-unknown-linux-musl)

## Feature flags
- `pty` does NOT exist. Use `term` for openpty/forkpty.
- `fs` required for: dup2, chdir, pipe (they're gated behind it).
- `poll` required for: poll() with PollFd/PollFlags.
- Current features: `["process", "signal", "term", "ioctl", "poll", "fs"]`

## pipe() returns OwnedFd
`nix::unistd::pipe()` returns `(OwnedFd, OwnedFd)` in 0.29. Extract raw fds BEFORE fork with `as_raw_fd()`, use raw fds in child, `mem::forget()` the OwnedFds you closed raw.

## close() expects RawFd
`nix::unistd::close()` takes `RawFd` (i32), not `OwnedFd`. Use `as_raw_fd()` or `libc::close()` directly.

## PollTimeout
`PollTimeout::from(100u16)` — only accepts u16 or u8, NOT i32. Don't use `nix::libc::c_int::from()`.

## sockaddr_vm (musl libc)
Musl's `sockaddr_vm` differs from glibc — no `svm_flags` or `svm_zero` fields in some versions. Always use `mem::zeroed()` + individual field assignment, never struct literal.

## openpty
`nix::pty::openpty(Some(&winsize), None)` returns `OpenptyResult { master, slave }` as raw fds (RawFd), not OwnedFd. Wrap with `OwnedFd::from_raw_fd()` in parent, use raw in child.

## signal handler cast
`libc::signal()` handler argument needs: `handler_fn as *const () as usize` — the intermediate `*const ()` cast is required since Rust 1.94.

## ioctl TIOCSCTTY / TIOCSWINSZ
Use `nix::libc::TIOCSCTTY as _` and `nix::libc::TIOCSWINSZ as _` — the `as _` lets the compiler infer the target type for the platform's ioctl request type.
