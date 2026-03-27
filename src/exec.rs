// Exec engine: fork/exec with pipe-based I/O (non-TTY) and PTY (TTY)

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, OwnedFd};

#[cfg(target_os = "linux")]
use nix::unistd::{self, ForkResult, Pid};

#[cfg(target_os = "linux")]
use crate::proto;

/// A non-TTY child process with pipe-based I/O.
#[cfg(target_os = "linux")]
pub struct Child {
    pub pid: Pid,
    pub stdin_fd: OwnedFd,
    pub stdout_fd: OwnedFd,
    pub stderr_fd: OwnedFd,
}

/// Fork and exec a command with pipe-based stdin/stdout/stderr.
#[cfg(target_os = "linux")]
pub fn exec_non_tty(req: &proto::ExecRequest) -> io::Result<Child> {
    use nix::unistd::{close, dup2, execvp, pipe};

    if req.cmd.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }

    let stdin_pipe = pipe().map_err(io::Error::other)?;
    let stdout_pipe = pipe().map_err(io::Error::other)?;
    let stderr_pipe = pipe().map_err(io::Error::other)?;

    // Extract raw fds before fork — OwnedFd doesn't cross fork safely
    let stdin_r = stdin_pipe.0.as_raw_fd();
    let stdin_w = stdin_pipe.1.as_raw_fd();
    let stdout_r = stdout_pipe.0.as_raw_fd();
    let stdout_w = stdout_pipe.1.as_raw_fd();
    let stderr_r = stderr_pipe.0.as_raw_fd();
    let stderr_w = stderr_pipe.1.as_raw_fd();

    let cmd_cstrs: Vec<CString> = req
        .cmd
        .iter()
        .map(|s| CString::new(s.as_str()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e)))
        .collect::<io::Result<Vec<_>>>()?;

    match unsafe { unistd::fork() } {
        Ok(ForkResult::Child) => {
            // Child: set up pipes, env, cwd, exec
            let _ = close(stdin_w);  // close write end of stdin
            let _ = close(stdout_r); // close read end of stdout
            let _ = close(stderr_r); // close read end of stderr

            let _ = dup2(stdin_r, 0);   // stdin
            let _ = dup2(stdout_w, 1);  // stdout
            let _ = dup2(stderr_w, 2);  // stderr

            let _ = close(stdin_r);
            let _ = close(stdout_w);
            let _ = close(stderr_w);

            // Set cwd
            if !req.cwd.is_empty()
                && unistd::chdir(req.cwd.as_str()).is_err() {
                std::process::exit(127);
            }

            // Set environment
            setup_env(&req.env);

            // Exec
            let _ = execvp(&cmd_cstrs[0], &cmd_cstrs);
            // If we get here, exec failed
            std::process::exit(127);
        }
        Ok(ForkResult::Parent { child }) => {
            // Parent: close child ends of pipes, forget OwnedFds we'll use raw
            // We need to close the child-side fds and keep the parent-side
            let _ = close(stdin_r);  // close read end of stdin
            let _ = close(stdout_w); // close write end of stdout
            let _ = close(stderr_w); // close write end of stderr

            // Forget the OwnedFds to avoid double-close (we closed raw fds above)
            std::mem::forget(stdin_pipe.0);
            std::mem::forget(stdout_pipe.1);
            std::mem::forget(stderr_pipe.1);

            // Transfer ownership of the parent-side fds
            // The OwnedFds for stdin_pipe.1, stdout_pipe.0, stderr_pipe.0 are still valid
            Ok(Child {
                pid: child,
                stdin_fd: stdin_pipe.1,
                stdout_fd: stdout_pipe.0,
                stderr_fd: stderr_pipe.0,
            })
        }
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Set up the child process environment.
#[cfg(target_os = "linux")]
pub fn setup_env(env: &HashMap<String, String>) {
    // Clear existing env
    for (key, _) in std::env::vars() {
        std::env::remove_var(&key);
    }

    // Minimal defaults
    std::env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    std::env::set_var("HOME", "/root");
    std::env::set_var("TERM", "xterm-256color");

    // User-provided env overrides defaults
    for (k, v) in env {
        std::env::set_var(k, v);
    }
}

/// Wait for a child and return ExitStatus.
#[cfg(target_os = "linux")]
pub fn wait_child(pid: Pid) -> io::Result<proto::ExitStatus> {
    use nix::sys::wait::{waitpid, WaitStatus};

    match waitpid(pid, None) {
        Ok(WaitStatus::Exited(_, code)) => Ok(proto::ExitStatus {
            code,
            signal: 0,
        }),
        Ok(WaitStatus::Signaled(_, sig, _)) => Ok(proto::ExitStatus {
            code: -1,
            signal: sig as i32,
        }),
        Ok(other) => Err(io::Error::other(
            format!("unexpected wait status: {:?}", other),
        )),
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Forward a signal to a child process.
#[cfg(target_os = "linux")]
pub fn send_signal(pid: Pid, signum: i32) -> io::Result<()> {
    if !(1..=31).contains(&signum) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid signal number: {}", signum),
        ));
    }
    let sig = nix::sys::signal::Signal::try_from(signum)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    nix::sys::signal::kill(pid, sig)
        .map_err(io::Error::other)
}


#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    fn make_req(cmd: &[&str]) -> proto::ExecRequest {
        proto::ExecRequest {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        }
    }

    // --- M3.1: exec_non_tty tests ---

    #[test]
    fn exec_echo_hello() {
        let child = exec_non_tty(&make_req(&["echo", "hello"])).unwrap();
        let mut stdout = unsafe { std::fs::File::from_raw_fd(child.stdout_fd.as_raw_fd()) };
        std::mem::forget(child.stdout_fd); // avoid double-close
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert_eq!(output, "hello\n");
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 0);
    }

    #[test]
    fn exec_with_cwd() {
        let mut req = make_req(&["pwd"]);
        req.cwd = "/tmp".into();
        let child = exec_non_tty(&req).unwrap();
        let mut stdout = unsafe { std::fs::File::from_raw_fd(child.stdout_fd.as_raw_fd()) };
        std::mem::forget(child.stdout_fd);
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert_eq!(output.trim(), "/tmp");
        let _ = wait_child(child.pid);
    }

    #[test]
    fn exec_nonexistent_command() {
        let child = exec_non_tty(&make_req(&["nonexistent_binary_xyz"])).unwrap();
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 127, "nonexistent command should exit 127");
    }

    // --- M3.2: stdin pipe tests ---

    #[test]
    fn exec_cat_stdin() {
        let child = exec_non_tty(&make_req(&["cat"])).unwrap();
        let mut stdin = unsafe { std::fs::File::from_raw_fd(child.stdin_fd.as_raw_fd()) };
        std::mem::forget(child.stdin_fd);
        stdin.write_all(b"test\n").unwrap();
        drop(stdin); // close stdin -> cat exits

        let mut stdout = unsafe { std::fs::File::from_raw_fd(child.stdout_fd.as_raw_fd()) };
        std::mem::forget(child.stdout_fd);
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert_eq!(output, "test\n");
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 0);
    }

    #[test]
    fn exec_wc_stdin() {
        let child = exec_non_tty(&make_req(&["wc", "-l"])).unwrap();
        let mut stdin = unsafe { std::fs::File::from_raw_fd(child.stdin_fd.as_raw_fd()) };
        std::mem::forget(child.stdin_fd);
        stdin.write_all(b"line1\nline2\nline3\n").unwrap();
        drop(stdin);

        let mut stdout = unsafe { std::fs::File::from_raw_fd(child.stdout_fd.as_raw_fd()) };
        std::mem::forget(child.stdout_fd);
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert_eq!(output.trim(), "3");
        let _ = wait_child(child.pid);
    }

    // --- M3.3: signal forwarding tests ---

    #[test]
    fn signal_term() {
        let child = exec_non_tty(&make_req(&["sleep", "60"])).unwrap();
        send_signal(child.pid, 15).unwrap(); // SIGTERM
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, -1);
        assert_eq!(status.signal, 15);
    }

    #[test]
    fn signal_kill() {
        let child = exec_non_tty(&make_req(&["sleep", "60"])).unwrap();
        send_signal(child.pid, 9).unwrap(); // SIGKILL
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, -1);
        assert_eq!(status.signal, 9);
    }

    // --- M3.4: exit status tests ---

    #[test]
    fn exit_code_true() {
        let child = exec_non_tty(&make_req(&["true"])).unwrap();
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 0);
        assert_eq!(status.signal, 0);
    }

    #[test]
    fn exit_code_false() {
        let child = exec_non_tty(&make_req(&["false"])).unwrap();
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 1);
        assert_eq!(status.signal, 0);
    }

    #[test]
    fn exit_code_42() {
        let child = exec_non_tty(&make_req(&["sh", "-c", "exit 42"])).unwrap();
        let status = wait_child(child.pid).unwrap();
        assert_eq!(status.code, 42);
        assert_eq!(status.signal, 0);
    }
}
