// TLV framing layer
//
// Frame format: [1 byte type][4 byte u32 LE length][payload]
// Max payload: 1 MB

use std::io::{self, Read, Write};

use prost::Message;

/// Maximum payload size: 1 MB
pub const MAX_PAYLOAD: u32 = 1_048_576;

/// Header size: 1 byte type + 4 bytes length
pub const HEADER_SIZE: usize = 5;

// Host -> Guest
pub const EXEC_REQUEST: u8 = 0x01;
pub const STDIN_DATA: u8 = 0x02;
pub const STDIN_EOF: u8 = 0x03;
pub const WINDOW_RESIZE: u8 = 0x04;
pub const SIGNAL: u8 = 0x05;

// Guest -> Host
pub const EXEC_STARTED: u8 = 0x10;
pub const STDOUT_DATA: u8 = 0x11;
pub const STDERR_DATA: u8 = 0x12;
pub const EXIT_STATUS: u8 = 0x13;
pub const EXEC_ERROR: u8 = 0x14;

/// Write a TLV frame: [type: u8][length: u32 LE][payload]
pub fn write_frame(w: &mut impl Write, type_byte: u8, payload: &[u8]) -> io::Result<()> {
    let len = payload.len();
    if len > MAX_PAYLOAD as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("payload too large: {} > {}", len, MAX_PAYLOAD),
        ));
    }
    w.write_all(&[type_byte])?;
    w.write_all(&(len as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Read a TLV frame, returning (type_byte, payload).
pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; HEADER_SIZE];
    match r.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }
        Err(e) => return Err(e),
    }
    let type_byte = hdr[0];
    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame too large: {} > {}", len, MAX_PAYLOAD),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok((type_byte, payload))
}

/// Write a protobuf message as a TLV frame.
pub fn write_proto<M: Message>(w: &mut impl Write, type_byte: u8, msg: &M) -> io::Result<()> {
    let payload = msg.encode_to_vec();
    write_frame(w, type_byte, &payload)
}

/// Read a TLV frame and decode as protobuf. Verifies the type byte matches.
#[allow(dead_code)]
pub fn read_proto<M: Message + Default>(r: &mut impl Read, expected_type: u8) -> io::Result<M> {
    let (type_byte, payload) = read_frame(r)?;
    if type_byte != expected_type {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected type 0x{:02x}, got 0x{:02x}", expected_type, type_byte),
        ));
    }
    M::decode(payload.as_slice()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use crate::proto;

    // --- M1.2: type constant tests ---

    #[test]
    fn host_to_guest_constants() {
        assert_eq!(EXEC_REQUEST, 0x01);
        assert_eq!(STDIN_DATA, 0x02);
        assert_eq!(STDIN_EOF, 0x03);
        assert_eq!(WINDOW_RESIZE, 0x04);
        assert_eq!(SIGNAL, 0x05);
    }

    #[test]
    fn guest_to_host_constants() {
        assert_eq!(EXEC_STARTED, 0x10);
        assert_eq!(STDOUT_DATA, 0x11);
        assert_eq!(STDERR_DATA, 0x12);
        assert_eq!(EXIT_STATUS, 0x13);
        assert_eq!(EXEC_ERROR, 0x14);
    }

    // --- M1.3: protobuf round-trip tests ---

    #[test]
    fn round_trip_exec_request_full() {
        let req = proto::ExecRequest {
            cmd: vec!["/bin/sh".into(), "-c".into(), "echo hello".into()],
            env: [("HOME".into(), "/root".into()), ("PATH".into(), "/usr/bin".into())]
                .into_iter()
                .collect(),
            cwd: "/tmp".into(),
            tty: true,
            rows: 24,
            cols: 80,
        };
        let encoded = req.encode_to_vec();
        let decoded = proto::ExecRequest::decode(encoded.as_slice()).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn round_trip_exec_request_minimal() {
        let req = proto::ExecRequest {
            cmd: vec!["ls".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        };
        let encoded = req.encode_to_vec();
        let decoded = proto::ExecRequest::decode(encoded.as_slice()).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn round_trip_exit_status_normal() {
        let status = proto::ExitStatus { code: 0, signal: 0 };
        let encoded = status.encode_to_vec();
        let decoded = proto::ExitStatus::decode(encoded.as_slice()).unwrap();
        assert_eq!(status, decoded);
    }

    #[test]
    fn round_trip_exit_status_signal() {
        let status = proto::ExitStatus { code: -1, signal: 9 };
        let encoded = status.encode_to_vec();
        let decoded = proto::ExitStatus::decode(encoded.as_slice()).unwrap();
        assert_eq!(status, decoded);
    }

    #[test]
    fn round_trip_exec_error() {
        let err = proto::ExecError {
            error: "command not found: foobar".into(),
        };
        let encoded = err.encode_to_vec();
        let decoded = proto::ExecError::decode(encoded.as_slice()).unwrap();
        assert_eq!(err, decoded);
    }

    #[test]
    fn round_trip_window_resize() {
        let resize = proto::WindowResize { rows: 48, cols: 120 };
        let encoded = resize.encode_to_vec();
        let decoded = proto::WindowResize::decode(encoded.as_slice()).unwrap();
        assert_eq!(resize, decoded);
    }

    #[test]
    fn round_trip_signal() {
        let sig = proto::Signal { signal: 15 };
        let encoded = sig.encode_to_vec();
        let decoded = proto::Signal::decode(encoded.as_slice()).unwrap();
        assert_eq!(sig, decoded);
    }

    #[test]
    fn round_trip_exec_started() {
        let started = proto::ExecStarted { pid: 42 };
        let encoded = started.encode_to_vec();
        let decoded = proto::ExecStarted::decode(encoded.as_slice()).unwrap();
        assert_eq!(started, decoded);
    }

    // --- M2.1: write_frame tests ---

    #[test]
    fn write_frame_basic() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 0x01, b"hello").unwrap();
        // type(1) + len(4) + payload(5) = 10
        assert_eq!(buf.len(), 10);
        assert_eq!(buf[0], 0x01);
        assert_eq!(u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]), 5);
        assert_eq!(&buf[5..], b"hello");
    }

    #[test]
    fn write_frame_empty_payload() {
        let mut buf = Vec::new();
        write_frame(&mut buf, STDIN_EOF, &[]).unwrap();
        assert_eq!(buf.len(), 5);
        assert_eq!(buf[0], STDIN_EOF);
        assert_eq!(u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]), 0);
    }

    #[test]
    fn write_frame_reject_oversized() {
        let big = vec![0u8; MAX_PAYLOAD as usize + 1];
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, 0x01, &big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // --- M2.2: read_frame tests ---

    #[test]
    fn read_frame_basic() {
        let mut buf = Vec::new();
        write_frame(&mut buf, STDOUT_DATA, b"world").unwrap();
        let mut cursor = Cursor::new(buf);
        let (t, payload) = read_frame(&mut cursor).unwrap();
        assert_eq!(t, STDOUT_DATA);
        assert_eq!(payload, b"world");
    }

    #[test]
    fn read_frame_empty_payload() {
        let mut buf = Vec::new();
        write_frame(&mut buf, STDIN_EOF, &[]).unwrap();
        let mut cursor = Cursor::new(buf);
        let (t, payload) = read_frame(&mut cursor).unwrap();
        assert_eq!(t, STDIN_EOF);
        assert!(payload.is_empty());
    }

    #[test]
    fn read_frame_reject_oversized_length() {
        // Craft a header with length > MAX_PAYLOAD
        let mut buf = vec![0x01];
        buf.extend_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        let mut cursor = Cursor::new(buf);
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_frame_eof_on_empty() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_frame_partial_header() {
        let mut cursor = Cursor::new(vec![0x01, 0x00]); // only 2 of 5 header bytes
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_frame_max_payload() {
        let data = vec![0xAA; MAX_PAYLOAD as usize];
        let mut buf = Vec::new();
        write_frame(&mut buf, 0x01, &data).unwrap();
        let mut cursor = Cursor::new(buf);
        let (t, payload) = read_frame(&mut cursor).unwrap();
        assert_eq!(t, 0x01);
        assert_eq!(payload.len(), MAX_PAYLOAD as usize);
    }

    // --- M2.3: proto helper tests ---

    #[test]
    fn write_read_proto_round_trip() {
        let req = proto::ExecRequest {
            cmd: vec!["echo".into(), "hi".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        };
        let mut buf = Vec::new();
        write_proto(&mut buf, EXEC_REQUEST, &req).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded: proto::ExecRequest = read_proto(&mut cursor, EXEC_REQUEST).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn read_proto_type_mismatch() {
        let status = proto::ExitStatus { code: 0, signal: 0 };
        let mut buf = Vec::new();
        write_proto(&mut buf, EXIT_STATUS, &status).unwrap();
        let mut cursor = Cursor::new(buf);
        let err = read_proto::<proto::ExitStatus>(&mut cursor, EXEC_REQUEST).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("expected type"), "{}", err);
    }

    #[test]
    fn raw_bytes_round_trip() {
        let data = b"some raw stdout data";
        let mut buf = Vec::new();
        write_frame(&mut buf, STDOUT_DATA, data).unwrap();
        let mut cursor = Cursor::new(buf);
        let (t, payload) = read_frame(&mut cursor).unwrap();
        assert_eq!(t, STDOUT_DATA);
        assert_eq!(payload, data);
    }

    // --- M2.4: bidirectional TCP test ---

    #[test]
    fn bidirectional_conversation_over_tcp() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();

            // Server reads EXEC_REQUEST
            let req: proto::ExecRequest = read_proto(&mut conn, EXEC_REQUEST).unwrap();
            assert_eq!(req.cmd, vec!["echo", "hello"]);

            // Server sends EXEC_STARTED
            write_proto(&mut conn, EXEC_STARTED, &proto::ExecStarted { pid: 123 }).unwrap();

            // Server sends STDOUT_DATA (raw bytes)
            write_frame(&mut conn, STDOUT_DATA, b"hello\n").unwrap();

            // Server sends EXIT_STATUS
            write_proto(
                &mut conn,
                EXIT_STATUS,
                &proto::ExitStatus { code: 0, signal: 0 },
            )
            .unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();

        // Client sends EXEC_REQUEST
        let req = proto::ExecRequest {
            cmd: vec!["echo".into(), "hello".into()],
            env: Default::default(),
            cwd: String::new(),
            tty: false,
            rows: 0,
            cols: 0,
        };
        write_proto(&mut client, EXEC_REQUEST, &req).unwrap();

        // Client reads EXEC_STARTED
        let started: proto::ExecStarted = read_proto(&mut client, EXEC_STARTED).unwrap();
        assert_eq!(started.pid, 123);

        // Client reads STDOUT_DATA
        let (t, payload) = read_frame(&mut client).unwrap();
        assert_eq!(t, STDOUT_DATA);
        assert_eq!(payload, b"hello\n");

        // Client reads EXIT_STATUS
        let status: proto::ExitStatus = read_proto(&mut client, EXIT_STATUS).unwrap();
        assert_eq!(status.code, 0);
        assert_eq!(status.signal, 0);

        server.join().unwrap();
    }
}
