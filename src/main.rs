mod proto;
mod wire;
mod exec;
mod session;
mod listener;

const DEFAULT_PORT: u32 = 5100;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: bentos-execd [OPTIONS]");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --port PORT       vsock port to listen on (default: {})", DEFAULT_PORT);
        eprintln!("  --tcp ADDR        use TCP instead of vsock (e.g. 127.0.0.1:5100)");
        eprintln!("  --log-level LEVEL log level: error, warn, info, debug, trace (default: info)");
        eprintln!("  --version         print version and exit");
        eprintln!();
        eprintln!("On Linux, defaults to vsock. Use --tcp for development/Docker testing.");
        eprintln!("On non-Linux, --tcp is required (vsock unavailable).");
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        eprintln!("bentos-execd {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let tcp_addr = parse_arg(&args, "--tcp");

    let log_level = parse_arg(&args, "--log-level").unwrap_or_else(|| "info".to_string());
    std::env::set_var("RUST_LOG", &log_level);
    env_logger::init();

    log::info!("bentos-execd {} starting", env!("CARGO_PKG_VERSION"));

    // Install SIGTERM handler
    RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    unsafe {
        nix::libc::signal(nix::libc::SIGTERM, sigterm_handler as *const () as usize);
    }

    if let Some(addr) = tcp_addr {
        // TCP mode — works on all platforms (development, Docker testing)
        let tcp = listener::TcpListener::bind(&addr).unwrap_or_else(|e| {
            log::error!("failed to bind TCP {}: {}", addr, e);
            std::process::exit(1);
        });
        listener::serve(&tcp, &RUNNING);
    } else {
        // vsock mode — Linux production only
        #[cfg(target_os = "linux")]
        {
            let port = parse_arg(&args, "--port")
                .map(|s| s.parse::<u32>().expect("invalid port number"))
                .unwrap_or(DEFAULT_PORT);
            let vsock = listener::VsockListener::bind(port).unwrap_or_else(|e| {
                log::error!("failed to bind vsock port {}: {}", port, e);
                std::process::exit(1);
            });
            listener::serve(&vsock, &RUNNING);
        }

        #[cfg(not(target_os = "linux"))]
        {
            log::error!("vsock requires Linux — use --tcp ADDR for development");
            eprintln!("bentos-execd: vsock requires Linux. Use --tcp 127.0.0.1:5100 for development.");
            std::process::exit(1);
        }
    }

    log::info!("waiting for active sessions to drain...");
    std::thread::sleep(std::time::Duration::from_secs(5));
    log::info!("bentos-execd stopped");
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(target_os = "linux")]
extern "C" fn sigterm_handler(_sig: i32) {
    RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
}
