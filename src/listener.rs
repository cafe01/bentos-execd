// Abstract listener — socket-type-agnostic accept loop
//
// TCP backend for testing on macOS, vsock backend for production on Linux.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

use crate::session;

/// A connection accepted by a listener.
pub trait Connection: Read + Write + AsRawFd + Send + 'static {
    fn peer_label(&self) -> String;
}

/// A listener that accepts connections.
pub trait Listener {
    type Conn: Connection;
    fn accept(&self) -> io::Result<Self::Conn>;
}

/// Run the accept loop on any listener.
pub fn serve<L: Listener>(listener: &L, shutdown: &std::sync::atomic::AtomicBool) {
    loop {
        if !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            log::info!("shutting down — signal received");
            break;
        }

        match listener.accept() {
            Ok(conn) => {
                let label = conn.peer_label();
                log::info!("connection from {}", label);
                std::thread::spawn(move || {
                    let mut conn = conn;
                    if let Err(e) = session::handle_connection(&mut conn) {
                        log::error!("session error: {}", e);
                    }
                    log::info!("connection from {} closed", label);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                log::error!("accept failed: {}", e);
                continue;
            }
        }
    }
}

// --- TCP backend (always available, used for testing) ---

impl Connection for std::net::TcpStream {
    fn peer_label(&self) -> String {
        self.peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".into())
    }
}

pub struct TcpListener {
    inner: std::net::TcpListener,
}

impl TcpListener {
    pub fn bind(addr: &str) -> io::Result<Self> {
        let inner = std::net::TcpListener::bind(addr)?;
        log::info!("bentos-execd listening on TCP {}", inner.local_addr()?);
        Ok(Self { inner })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.inner.local_addr()
    }
}

impl Listener for TcpListener {
    type Conn = std::net::TcpStream;
    fn accept(&self) -> io::Result<std::net::TcpStream> {
        let (stream, _) = self.inner.accept()?;
        Ok(stream)
    }
}

// --- vsock backend (Linux only) ---

#[cfg(target_os = "linux")]
pub struct VsockStream {
    fd: std::os::fd::OwnedFd,
    cid: u32,
}

#[cfg(target_os = "linux")]
impl VsockStream {
    fn new(fd: std::os::fd::OwnedFd, cid: u32) -> Self {
        Self { fd, cid }
    }
}

#[cfg(target_os = "linux")]
impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            nix::libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len())
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

#[cfg(target_os = "linux")]
impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe {
            nix::libc::write(self.fd.as_raw_fd(), buf.as_ptr() as *const _, buf.len())
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // vsock is stream-based, no buffering
    }
}

#[cfg(target_os = "linux")]
impl AsRawFd for VsockStream {
    fn as_raw_fd(&self) -> std::os::raw::c_int {
        self.fd.as_raw_fd()
    }
}

#[cfg(target_os = "linux")]
impl Connection for VsockStream {
    fn peer_label(&self) -> String {
        format!("CID {}", self.cid)
    }
}

#[cfg(target_os = "linux")]
pub struct VsockListener {
    fd: std::os::fd::OwnedFd,
    _port: u32,
}

#[cfg(target_os = "linux")]
impl VsockListener {
    pub fn bind(port: u32) -> io::Result<Self> {
        use std::os::fd::FromRawFd;

        let fd = unsafe {
            nix::libc::socket(nix::libc::AF_VSOCK, nix::libc::SOCK_STREAM, 0)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };

        let mut addr: nix::libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        addr.svm_family = nix::libc::AF_VSOCK as u16;
        addr.svm_port = port;
        addr.svm_cid = nix::libc::VMADDR_CID_ANY;

        let ret = unsafe {
            nix::libc::bind(
                fd.as_raw_fd(),
                &addr as *const nix::libc::sockaddr_vm as *const nix::libc::sockaddr,
                std::mem::size_of::<nix::libc::sockaddr_vm>() as u32,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        let ret = unsafe { nix::libc::listen(fd.as_raw_fd(), 16) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        log::info!("bentos-execd listening on vsock port {}", port);
        Ok(Self { fd, _port: port })
    }
}

#[cfg(target_os = "linux")]
impl Listener for VsockListener {
    type Conn = VsockStream;
    fn accept(&self) -> io::Result<VsockStream> {
        use std::os::fd::FromRawFd;

        let mut client_addr: nix::libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        let mut addr_len = std::mem::size_of::<nix::libc::sockaddr_vm>() as u32;

        let client_fd = unsafe {
            nix::libc::accept(
                self.fd.as_raw_fd(),
                &mut client_addr as *mut nix::libc::sockaddr_vm as *mut nix::libc::sockaddr,
                &mut addr_len,
            )
        };
        if client_fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(client_fd) };
        Ok(VsockStream::new(fd, client_addr.svm_cid))
    }
}

// --- Integration tests: full path through TCP listener ---

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{proto, wire};

    /// Full integration: listener → accept → handle_connection → exec → TLV response
    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_listener_full_exec() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let conn = listener.accept().unwrap();
            let mut conn = conn;
            session::handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = proto::ExecRequest {
            cmd: vec!["echo".into(), "integration".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        };
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();
        assert!(started.pid > 0);

        let (t, payload) = wire::read_frame(&mut client).unwrap();
        assert_eq!(t, wire::STDOUT_DATA);
        assert_eq!(payload, b"integration\n");

        let status: proto::ExitStatus =
            wire::read_proto(&mut client, wire::EXIT_STATUS).unwrap();
        assert_eq!(status.code, 0);

        server.join().unwrap();
    }

    /// Full integration: TCP listener with TTY session
    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_listener_tty_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let conn = listener.accept().unwrap();
            let mut conn = conn;
            session::handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let req = proto::ExecRequest {
            cmd: vec!["/bin/sh".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: true,
            rows: 24,
            cols: 80,
        };
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        let _started: proto::ExecStarted =
            wire::read_proto(&mut client, wire::EXEC_STARTED).unwrap();

        // Send a command through the TTY
        wire::write_frame(&mut client, wire::STDIN_DATA, b"echo tty-test\n").unwrap();

        // Read until we see output
        let mut all = Vec::new();
        for _ in 0..20 {
            let (t, payload) = wire::read_frame(&mut client).unwrap();
            if t == wire::STDOUT_DATA {
                all.extend_from_slice(&payload);
                if String::from_utf8_lossy(&all).contains("tty-test") {
                    break;
                }
            }
        }
        assert!(String::from_utf8_lossy(&all).contains("tty-test"));

        // Clean exit
        wire::write_frame(&mut client, wire::STDIN_DATA, b"exit\n").unwrap();
        loop {
            let (t, _) = wire::read_frame(&mut client).unwrap();
            if t == wire::EXIT_STATUS { break; }
        }
        server.join().unwrap();
    }

    /// serve() loop handles multiple sequential connections
    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_serve_multiple_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let shutdown_clone = shutdown.clone();

        let _server = std::thread::spawn(move || {
            serve(&listener, &shutdown_clone);
        });

        // Connection 1
        {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            let req = proto::ExecRequest {
                cmd: vec!["echo".into(), "first".into()],
                env: Default::default(),
                cwd: String::new(),
                tty: false,
                rows: 0,
                cols: 0,
            };
            wire::write_proto(&mut c, wire::EXEC_REQUEST, &req).unwrap();
            let _: proto::ExecStarted = wire::read_proto(&mut c, wire::EXEC_STARTED).unwrap();
            let (t, payload) = wire::read_frame(&mut c).unwrap();
            assert_eq!(t, wire::STDOUT_DATA);
            assert_eq!(payload, b"first\n");
            let _: proto::ExitStatus = wire::read_proto(&mut c, wire::EXIT_STATUS).unwrap();
        }

        // Connection 2
        {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            let req = proto::ExecRequest {
                cmd: vec!["echo".into(), "second".into()],
                env: Default::default(),
                cwd: String::new(),
                tty: false,
                rows: 0,
                cols: 0,
            };
            wire::write_proto(&mut c, wire::EXEC_REQUEST, &req).unwrap();
            let _: proto::ExecStarted = wire::read_proto(&mut c, wire::EXEC_STARTED).unwrap();
            let (t, payload) = wire::read_frame(&mut c).unwrap();
            assert_eq!(t, wire::STDOUT_DATA);
            assert_eq!(payload, b"second\n");
            let _: proto::ExitStatus = wire::read_proto(&mut c, wire::EXIT_STATUS).unwrap();
        }

        // Shutdown
        shutdown.store(false, std::sync::atomic::Ordering::SeqCst);
        // Poke the listener to unblock accept
        let _ = std::net::TcpStream::connect(addr);
        // Give the server thread time to notice shutdown
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    /// TCP listener integration test that works on macOS (no exec, just framing)
    #[test]
    fn tcp_listener_accept_works() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let conn = listener.accept().unwrap();
            assert!(!conn.peer_label().is_empty());
        });

        let _client = std::net::TcpStream::connect(addr).unwrap();
        server.join().unwrap();
    }

    /// M5.1: Full listener path on macOS — TCP listener → accept → handle_connection
    /// → EXEC_REQUEST parsed → EXEC_ERROR returned (exec requires Linux).
    /// Validates: listener abstraction, accept, TLV framing, protobuf decode, error response.
    #[test]
    fn tcp_listener_handle_connection_cross_platform() {
        use crate::session;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            session::handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();

        // Send a valid EXEC_REQUEST
        let req = proto::ExecRequest {
            cmd: vec!["echo".into(), "cross-platform".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        };
        wire::write_proto(&mut client, wire::EXEC_REQUEST, &req).unwrap();

        // On Linux: expect EXEC_STARTED + output + EXIT_STATUS
        // On non-Linux: expect EXEC_ERROR
        let (t, payload) = wire::read_frame(&mut client).unwrap();

        #[cfg(target_os = "linux")]
        {
            assert_eq!(t, wire::EXEC_STARTED, "Linux should start exec");
            let started = <proto::ExecStarted as prost::Message>::decode(payload.as_slice()).unwrap();
            assert!(started.pid > 0);
            // Read remaining output + exit status
            loop {
                let (t, _) = wire::read_frame(&mut client).unwrap();
                if t == wire::EXIT_STATUS { break; }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(t, wire::EXEC_ERROR, "non-Linux should return EXEC_ERROR");
            let err = <proto::ExecError as prost::Message>::decode(payload.as_slice()).unwrap();
            assert!(err.error.contains("requires Linux"), "error: {}", err.error);
        }

        server.join().unwrap();
    }

    /// M5.4: End-to-end integration — start serve() loop, execute multiple commands
    /// through the full listener → accept → handle_connection → exec path over TCP.
    /// Validates: serve() accept loop, concurrent connections, non-TTY + TTY dispatch.
    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_serve_end_to_end_integration() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let shutdown_clone = shutdown.clone();

        let _server = std::thread::spawn(move || {
            serve(&listener, &shutdown_clone);
        });

        // Non-TTY: execute 'echo e2e-test' and verify output
        {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            let req = proto::ExecRequest {
                cmd: vec!["echo".into(), "e2e-test".into()],
                env: Default::default(),
                cwd: String::new(),
                tty: false,
                rows: 0,
                cols: 0,
            };
            wire::write_proto(&mut c, wire::EXEC_REQUEST, &req).unwrap();
            let started: proto::ExecStarted =
                wire::read_proto(&mut c, wire::EXEC_STARTED).unwrap();
            assert!(started.pid > 0, "should get valid PID");
            let (t, payload) = wire::read_frame(&mut c).unwrap();
            assert_eq!(t, wire::STDOUT_DATA);
            assert_eq!(payload, b"e2e-test\n");
            let status: proto::ExitStatus =
                wire::read_proto(&mut c, wire::EXIT_STATUS).unwrap();
            assert_eq!(status.code, 0);
        }

        // TTY: start shell, run command, verify output, exit
        {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            let req = proto::ExecRequest {
                cmd: vec!["/bin/sh".into()],
                env: Default::default(),
                cwd: String::new(),
                tty: true,
                rows: 24,
                cols: 80,
            };
            wire::write_proto(&mut c, wire::EXEC_REQUEST, &req).unwrap();
            let _started: proto::ExecStarted =
                wire::read_proto(&mut c, wire::EXEC_STARTED).unwrap();

            wire::write_frame(&mut c, wire::STDIN_DATA, b"echo e2e-tty\n").unwrap();

            let mut all = Vec::new();
            for _ in 0..20 {
                let (t, payload) = wire::read_frame(&mut c).unwrap();
                if t == wire::STDOUT_DATA {
                    all.extend_from_slice(&payload);
                    if String::from_utf8_lossy(&all).contains("e2e-tty") {
                        break;
                    }
                }
            }
            assert!(
                String::from_utf8_lossy(&all).contains("e2e-tty"),
                "TTY should echo command output"
            );

            wire::write_frame(&mut c, wire::STDIN_DATA, b"exit\n").unwrap();
            loop {
                let (t, _) = wire::read_frame(&mut c).unwrap();
                if t == wire::EXIT_STATUS { break; }
            }
        }

        // Invalid request: should get EXEC_ERROR
        {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            wire::write_frame(&mut c, wire::STDIN_DATA, b"bad").unwrap();
            let (t, payload) = wire::read_frame(&mut c).unwrap();
            assert_eq!(t, wire::EXEC_ERROR);
            let err = <proto::ExecError as prost::Message>::decode(payload.as_slice()).unwrap();
            assert!(err.error.contains("expected EXEC_REQUEST"));
        }

        shutdown.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(addr);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    /// M5.1: Invalid frame type through full listener path (cross-platform).
    #[test]
    fn tcp_listener_invalid_request_cross_platform() {
        use crate::session;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            session::handle_connection(&mut conn).unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();

        // Send wrong frame type
        wire::write_frame(&mut client, wire::STDIN_DATA, b"bad").unwrap();

        let (t, payload) = wire::read_frame(&mut client).unwrap();
        assert_eq!(t, wire::EXEC_ERROR);
        let err = <proto::ExecError as prost::Message>::decode(payload.as_slice()).unwrap();
        assert!(err.error.contains("expected EXEC_REQUEST"));

        server.join().unwrap();
    }
}
