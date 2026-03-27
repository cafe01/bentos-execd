// Per-connection protocol handler — bidirectional I/O between stream and child

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

use crate::proto;
use crate::wire;

#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};

#[cfg(target_os = "linux")]
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

#[cfg(target_os = "linux")]
use crate::exec;

/// Run a non-TTY session: bidirectional I/O between a stream and child pipes.
#[cfg(target_os = "linux")]
pub fn session_non_tty<S: Read + Write + AsRawFd>(
    stream: &mut S,
    child: &mut exec::Child,
) -> io::Result<proto::ExitStatus> {
    use std::os::fd::BorrowedFd;

    let mut buf = [0u8; 32768];
    let mut stdin_open = true;

    loop {
        let stream_fd = unsafe { BorrowedFd::borrow_raw(stream.as_raw_fd()) };
        let stdout_fd = unsafe { BorrowedFd::borrow_raw(child.stdout_fd.as_raw_fd()) };
        let stderr_fd = unsafe { BorrowedFd::borrow_raw(child.stderr_fd.as_raw_fd()) };

        let mut fds = [
            PollFd::new(stream_fd, PollFlags::POLLIN),
            PollFd::new(stdout_fd, PollFlags::POLLIN),
            PollFd::new(stderr_fd, PollFlags::POLLIN),
        ];

        let _ready = poll(&mut fds, PollTimeout::from(100u16))
            .map_err(|e| io::Error::other(e))?;

        // Check stream for incoming commands
        if let Some(revents) = fds[0].revents() {
            if revents.contains(PollFlags::POLLIN) {
                match wire::read_frame(stream) {
                    Ok((wire::STDIN_DATA, data)) if stdin_open => {
                        let mut f = unsafe { std::fs::File::from_raw_fd(child.stdin_fd.as_raw_fd()) };
                        let result = f.write_all(&data);
                        std::mem::forget(f);
                        result?;
                    }
                    Ok((wire::STDIN_EOF, _)) if stdin_open => {
                        stdin_open = false;
                        unsafe { nix::libc::close(child.stdin_fd.as_raw_fd()); }
                    }
                    Ok((wire::SIGNAL, data)) => {
                        let sig = <proto::Signal as prost::Message>::decode(data.as_slice())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        exec::send_signal(child.pid, sig.signal)?;
                    }
                    Ok((t, _)) => {
                        log::warn!("unexpected frame type in non-tty session: 0x{:02x}", t);
                    }
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        let _ = exec::send_signal(child.pid, 1);
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // Check child stdout
        if let Some(revents) = fds[1].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let mut f = unsafe { std::fs::File::from_raw_fd(child.stdout_fd.as_raw_fd()) };
                let n = f.read(&mut buf)?;
                std::mem::forget(f);
                if n > 0 {
                    wire::write_frame(stream, wire::STDOUT_DATA, &buf[..n])?;
                }
            }
            if revents.contains(PollFlags::POLLHUP) && !revents.contains(PollFlags::POLLIN) {
                // Child stdout closed and no more data — child is done
                let status = exec::wait_child(child.pid)?;
                wire::write_proto(stream, wire::EXIT_STATUS, &status)?;
                return Ok(status);
            }
        }

        // Check child stderr
        if let Some(revents) = fds[2].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let mut f = unsafe { std::fs::File::from_raw_fd(child.stderr_fd.as_raw_fd()) };
                let n = f.read(&mut buf)?;
                std::mem::forget(f);
                if n > 0 {
                    wire::write_frame(stream, wire::STDERR_DATA, &buf[..n])?;
                }
            }
        }
    }

    exec::wait_child(child.pid)
}

/// Handle a new connection: read ExecRequest, dispatch to appropriate session.
/// On non-Linux platforms, always returns EXEC_ERROR (exec engine requires Linux).
pub fn handle_connection<S: Read + Write + AsRawFd>(stream: &mut S) -> io::Result<()> {
    let (type_byte, payload) = wire::read_frame(stream)?;
    if type_byte != wire::EXEC_REQUEST {
        let err = proto::ExecError {
            error: format!("expected EXEC_REQUEST (0x01), got 0x{:02x}", type_byte),
        };
        wire::write_proto(stream, wire::EXEC_ERROR, &err)?;
        return Ok(());
    }

    let req = <proto::ExecRequest as prost::Message>::decode(payload.as_slice())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    #[cfg(target_os = "linux")]
    {
        if req.tty {
            match exec_tty_session(stream, &req) {
                Ok(_) => Ok(()),
                Err(e) => {
                    let err = proto::ExecError {
                        error: format!("tty exec failed: {}", e),
                    };
                    let _ = wire::write_proto(stream, wire::EXEC_ERROR, &err);
                    Ok(())
                }
            }
        } else {
            match exec::exec_non_tty(&req) {
                Ok(mut child) => {
                    let started = proto::ExecStarted { pid: child.pid.as_raw() as u32 };
                    wire::write_proto(stream, wire::EXEC_STARTED, &started)?;
                    let _ = session_non_tty(stream, &mut child);
                    Ok(())
                }
                Err(e) => {
                    let err = proto::ExecError {
                        error: format!("exec failed: {}", e),
                    };
                    wire::write_proto(stream, wire::EXEC_ERROR, &err)?;
                    Ok(())
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = req;
        let err = proto::ExecError {
            error: "exec engine requires Linux".into(),
        };
        wire::write_proto(stream, wire::EXEC_ERROR, &err)?;
        Ok(())
    }
}

// --- M4: TTY mode ---

/// A TTY child process with a PTY master fd.
#[cfg(target_os = "linux")]
pub struct TtyChild {
    pub pid: nix::unistd::Pid,
    pub master_fd: OwnedFd,
}

/// Fork with a PTY and exec a command (TTY mode).
#[cfg(target_os = "linux")]
pub fn exec_tty(req: &proto::ExecRequest) -> io::Result<TtyChild> {
    use nix::pty::{openpty, OpenptyResult};
    use nix::unistd::{dup2, execvp, setsid, ForkResult};
    use std::ffi::CString;

    if req.cmd.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }

    let ws = nix::libc::winsize {
        ws_row: if req.rows > 0 { req.rows as u16 } else { 24 },
        ws_col: if req.cols > 0 { req.cols as u16 } else { 80 },
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let OpenptyResult { master, slave } = openpty(Some(&ws), None)
        .map_err(|e| io::Error::other(e))?;

    let master_raw = master.as_raw_fd();
    let slave_raw = slave.as_raw_fd();

    let cmd_cstrs: Vec<CString> = req
        .cmd
        .iter()
        .map(|s| CString::new(s.as_str()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e)))
        .collect::<io::Result<Vec<_>>>()?;

    match unsafe { nix::unistd::fork() } {
        Ok(ForkResult::Child) => {
            // Close master in child
            unsafe { nix::libc::close(master_raw); }
            std::mem::forget(master);

            // Create new session, set controlling terminal
            let _ = setsid();
            unsafe {
                nix::libc::ioctl(slave_raw, nix::libc::TIOCSCTTY as _, 0);
            }

            // Redirect stdio to slave PTY
            let _ = dup2(slave_raw, 0);
            let _ = dup2(slave_raw, 1);
            let _ = dup2(slave_raw, 2);
            if slave_raw > 2 {
                unsafe { nix::libc::close(slave_raw); }
            }
            std::mem::forget(slave);

            // Set cwd
            if !req.cwd.is_empty()
                && nix::unistd::chdir(req.cwd.as_str()).is_err() {
                std::process::exit(127);
            }

            // Set environment
            exec::setup_env(&req.env);

            // Exec
            let _ = execvp(&cmd_cstrs[0], &cmd_cstrs);
            std::process::exit(127);
        }
        Ok(ForkResult::Parent { child }) => {
            // Close slave in parent
            drop(slave);
            Ok(TtyChild {
                pid: child,
                master_fd: master,
            })
        }
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Resize the PTY window.
#[cfg(target_os = "linux")]
pub fn resize_pty(master_fd: i32, rows: u16, cols: u16) -> io::Result<()> {
    let ws = nix::libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe { nix::libc::ioctl(master_fd, nix::libc::TIOCSWINSZ as _, &ws) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Run a TTY session: bidirectional I/O between a stream and PTY master.
#[cfg(target_os = "linux")]
pub fn session_tty<S: Read + Write + AsRawFd>(
    stream: &mut S,
    tty_child: &TtyChild,
) -> io::Result<proto::ExitStatus> {
    use std::os::fd::BorrowedFd;

    let mut buf = [0u8; 32768];
    let master_raw = tty_child.master_fd.as_raw_fd();

    loop {
        let stream_fd = unsafe { BorrowedFd::borrow_raw(stream.as_raw_fd()) };
        let master_fd = unsafe { BorrowedFd::borrow_raw(master_raw) };

        let mut fds = [
            PollFd::new(stream_fd, PollFlags::POLLIN),
            PollFd::new(master_fd, PollFlags::POLLIN),
        ];

        let _ready = poll(&mut fds, PollTimeout::from(100u16))
            .map_err(|e| io::Error::other(e))?;

        // Check stream for incoming
        if let Some(revents) = fds[0].revents() {
            if revents.contains(PollFlags::POLLIN) {
                match wire::read_frame(stream) {
                    Ok((wire::STDIN_DATA, data)) => {
                        let mut f = unsafe { std::fs::File::from_raw_fd(master_raw) };
                        let result = f.write_all(&data);
                        std::mem::forget(f);
                        result?;
                    }
                    Ok((wire::WINDOW_RESIZE, data)) => {
                        let resize = <proto::WindowResize as prost::Message>::decode(data.as_slice())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        resize_pty(master_raw, resize.rows as u16, resize.cols as u16)?;
                    }
                    Ok((wire::SIGNAL, data)) => {
                        let sig = <proto::Signal as prost::Message>::decode(data.as_slice())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        exec::send_signal(tty_child.pid, sig.signal)?;
                    }
                    Ok((t, _)) => {
                        log::warn!("unexpected frame type in tty session: 0x{:02x}", t);
                    }
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        let _ = exec::send_signal(tty_child.pid, 1);
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // Check PTY master for output
        if let Some(revents) = fds[1].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let mut f = unsafe { std::fs::File::from_raw_fd(master_raw) };
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        std::mem::forget(f);
                        wire::write_frame(stream, wire::STDOUT_DATA, &buf[..n])?;
                    }
                    _ => {
                        std::mem::forget(f);
                    }
                }
            }
            if revents.contains(PollFlags::POLLHUP) && !revents.contains(PollFlags::POLLIN) {
                let status = exec::wait_child(tty_child.pid)?;
                wire::write_proto(stream, wire::EXIT_STATUS, &status)?;
                return Ok(status);
            }
        }
    }

    exec::wait_child(tty_child.pid)
}

/// TTY session entry point from handle_connection.
#[cfg(target_os = "linux")]
fn exec_tty_session<S: Read + Write + AsRawFd>(
    stream: &mut S,
    req: &proto::ExecRequest,
) -> io::Result<()> {
    let tty_child = exec_tty(req)?;
    let started = proto::ExecStarted {
        pid: tty_child.pid.as_raw() as u32,
    };
    wire::write_proto(stream, wire::EXEC_STARTED, &started)?;
    let _ = session_tty(stream, &tty_child)?;
    Ok(())
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use prost::Message;
    use std::net::TcpListener;

    fn make_req(cmd: &[&str], tty: bool) -> proto::ExecRequest {
        proto::ExecRequest {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env: Default::default(),
            cwd: String::new(),
            tty,
            rows: 24,
            cols: 80,
        }
    }

    // --- M3.5: non-TTY session over TCP ---

    #[test]
    fn non_tty_session_echo() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = make_req(&["echo", "hello"], false);
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();
        assert!(started.pid > 0);

        let (t, payload) = wire::read_frame(&mut client).unwrap();
        assert_eq!(t, wire::STDOUT_DATA);
        assert_eq!(payload, b"hello\n");

        let status: proto::ExitStatus =
            wire::read_proto(&mut client, wire::EXIT_STATUS).unwrap();
        assert_eq!(status.code, 0);

        server.join().unwrap();
    }

    #[test]
    fn non_tty_session_stdout_stderr() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = make_req(&["sh", "-c", "echo out; echo err >&2"], false);
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let _started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();

        let mut got_stdout = false;
        let mut got_stderr = false;
        loop {
            let (t, payload) = wire::read_frame(&mut client).unwrap();
            match t {
                wire::STDOUT_DATA => {
                    if payload.contains(&b'o') { got_stdout = true; }
                }
                wire::STDERR_DATA => {
                    if payload.contains(&b'e') { got_stderr = true; }
                }
                wire::EXIT_STATUS => {
                    let status = proto::ExitStatus::decode(payload.as_slice()).unwrap();
                    assert_eq!(status.code, 0);
                    break;
                }
                _ => panic!("unexpected frame type: 0x{:02x}", t),
            }
        }
        assert!(got_stdout, "should have received stdout data");
        assert!(got_stderr, "should have received stderr data");

        server.join().unwrap();
    }

    #[test]
    fn handle_invalid_first_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        wire::write_frame(&mut client, wire::STDIN_DATA, b"bad").unwrap();

        let (t, payload) = wire::read_frame(&mut client).unwrap();
        assert_eq!(t, wire::EXEC_ERROR);
        let err = proto::ExecError::decode(payload.as_slice()).unwrap();
        assert!(err.error.contains("expected EXEC_REQUEST"));

        server.join().unwrap();
    }

    // --- M4.3: TTY session tests ---

    #[test]
    fn tty_session_echo() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = make_req(&["/bin/sh"], true);
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let _started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();

        // Send command
        wire::write_frame(&mut client, wire::STDIN_DATA, b"echo hello\n").unwrap();

        // Read output until we see "hello"
        let mut all_output = Vec::new();
        for _ in 0..20 {
            let (t, payload) = wire::read_frame(&mut client).unwrap();
            if t == wire::STDOUT_DATA {
                all_output.extend_from_slice(&payload);
                if String::from_utf8_lossy(&all_output).contains("hello") {
                    break;
                }
            }
        }
        assert!(
            String::from_utf8_lossy(&all_output).contains("hello"),
            "should see 'hello' in output"
        );

        // Exit shell
        wire::write_frame(&mut client, wire::STDIN_DATA, b"exit\n").unwrap();

        // Read until EXIT_STATUS
        loop {
            let (t, _payload) = wire::read_frame(&mut client).unwrap();
            if t == wire::EXIT_STATUS {
                break;
            }
        }

        server.join().unwrap();
    }

    // --- M4.4: dispatch tests ---

    #[test]
    fn dispatch_tty_true() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = make_req(&["true"], true);
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let _started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();

        // Read until EXIT_STATUS
        loop {
            let (t, _) = wire::read_frame(&mut client).unwrap();
            if t == wire::EXIT_STATUS { break; }
        }
        server.join().unwrap();
    }

    #[test]
    fn dispatch_tty_false() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = make_req(&["true"], false);
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();
        assert!(started.pid > 0);

        let status: proto::ExitStatus =
            wire::read_proto(&mut client, wire::EXIT_STATUS).unwrap();
        assert_eq!(status.code, 0);

        server.join().unwrap();
    }
}
