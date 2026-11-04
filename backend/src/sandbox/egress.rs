use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

fn destination(header: &str, allowed: &[String]) -> Result<String, String> {
    let mut lines = header.lines();
    let parts = lines.next().unwrap_or_default().split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || parts[0] != "CONNECT" || parts[2] != "HTTP/1.1" {
        return Err("Only HTTPS CONNECT is supported".into());
    }
    let host = parts[1].strip_suffix(":443").ok_or("Only TLS port 443 is allowed")?;
    if !allowed.iter().any(|allowed| allowed == host) {
        return Err("Destination is not an approved provider".into());
    }
    Ok(host.into())
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_documentation()
                && !ip.is_broadcast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 192 && b == 0)
        }
        IpAddr::V6(_) => false,
    }
}

fn tunnel(mut client: TcpStream, allowed: &[String]) -> Result<(), String> {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|_| client.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|e| e.to_string())?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        client.read_exact(&mut byte).map_err(|_| "Truncated proxy request")?;
        header.push(byte[0]);
        if header.len() > 8192 {
            return Err("Proxy headers too large".into());
        }
    }
    let host = destination(
        std::str::from_utf8(&header).map_err(|_| "Invalid proxy header")?,
        allowed,
    )?;
    let address = (host.as_str(), 443)
        .to_socket_addrs()
        .map_err(|_| "Provider DNS unavailable")?
        .find(|address| public_ip(address.ip()))
        .ok_or("Provider did not resolve to an allowed public address")?;
    let remote =
        TcpStream::connect_timeout(&address, Duration::from_secs(10)).map_err(|_| "Provider connection unavailable")?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .map_err(|_| "Proxy client disconnected")?;
    relay(client, remote, Duration::from_secs(60))
}

fn relay(mut client: TcpStream, mut remote: TcpStream, idle: Duration) -> Result<(), String> {
    for stream in [&client, &remote] {
        stream
            .set_read_timeout(Some(idle.min(Duration::from_secs(1))))
            .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(10))))
            .map_err(|e| e.to_string())?;
    }
    let mut upstream = remote.try_clone().map_err(|e| e.to_string())?;
    let mut downstream = client.try_clone().map_err(|e| e.to_string())?;
    let activity = Mutex::new(Instant::now());
    let copy = |reader: &mut TcpStream, writer: &mut TcpStream| -> std::io::Result<()> {
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if activity
                        .lock()
                        .map_err(|_| std::io::Error::other("Tunnel activity unavailable"))?
                        .elapsed()
                        >= idle
                    {
                        return Err(error);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            *activity
                .lock()
                .map_err(|_| std::io::Error::other("Tunnel activity unavailable"))? = Instant::now();
            writer.write_all(&buffer[..count])?;
        }
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = copy(&mut client, &mut upstream);
            let _ = upstream.shutdown(std::net::Shutdown::Both);
        });
        let _ = copy(&mut remote, &mut downstream);
        let _ = downstream.shutdown(std::net::Shutdown::Both);
    });
    Ok(())
}

/// Model providers runners may reach: Anthropic for Claude accounts; ChatGPT, the OpenAI API
/// and OpenAI's sign-in refresh for Codex accounts; and OpenCode's own inference for OpenCode
/// accounts. Private networks, and so local models, stay out.
const PROVIDER_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "chatgpt.com",
    "api.openai.com",
    "auth.openai.com",
    "opencode.ai",
];

const MAX_CONNECTIONS: usize = 48;

fn capacity() -> usize {
    static CAPACITY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAPACITY.get_or_init(|| {
        std::env::var("OCR_EGRESS_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| (1..=512).contains(v))
            .unwrap_or(MAX_CONNECTIONS)
    })
}

fn admit_connection(active: &AtomicUsize) -> bool {
    if active.fetch_add(1, Ordering::SeqCst) < capacity() {
        return true;
    }
    active.fetch_sub(1, Ordering::SeqCst);
    false
}

pub fn serve() -> Result<(), String> {
    let allowed = Arc::new(PROVIDER_HOSTS.iter().map(|host| host.to_string()).collect::<Vec<_>>());
    let active = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("0.0.0.0:8790").map_err(|e| e.to_string())?;
    for stream in listener.incoming() {
        let stream = stream.map_err(|e| e.to_string())?;
        if !admit_connection(&active) {
            eprintln!(
                "Provider proxy connection capacity reached ({}); connection refused",
                capacity()
            );
            continue;
        }
        let active = active.clone();
        let allowed = allowed.clone();
        std::thread::spawn(move || {
            let _ = tunnel(stream, &allowed);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        for stream in [&client, &server] {
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        (client, server)
    }

    #[test]
    fn concurrent_cli_startup_capacity_remains_bounded() {
        let active = AtomicUsize::new(0);
        for _ in 0..MAX_CONNECTIONS {
            assert!(admit_connection(&active));
        }
        assert!(!admit_connection(&active));
        assert_eq!(active.load(Ordering::SeqCst), MAX_CONNECTIONS);
        active.fetch_sub(1, Ordering::SeqCst);
        assert!(admit_connection(&active));
        std::thread::scope(|scope| {
            let mut peers = Vec::new();
            for _ in 0..MAX_CONNECTIONS {
                let (client_peer, client) = pair();
                let (remote, remote_peer) = pair();
                scope.spawn(move || relay(client, remote, Duration::from_secs(60)).unwrap());
                peers.push((client_peer, remote_peer));
            }
            for (client, remote) in &mut peers {
                client.write_all(b"request").unwrap();
                let mut bytes = [0; 7];
                remote.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"request");
                remote.write_all(b"response").unwrap();
                let mut bytes = [0; 8];
                client.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"response");
            }
            if let Ok(pids) = std::fs::read_to_string("/sys/fs/cgroup/pids.current") {
                let pids = pids.trim().parse::<usize>().unwrap();
                assert!(pids <= 128, "Proxy capacity exceeds its existing PID budget");
                println!("PROXY_CAPACITY_VALIDATED connections={MAX_CONNECTIONS} pids={pids}");
            }
        });
    }

    #[test]
    fn active_response_survives_a_silent_request_direction() {
        for reverse in [false, true] {
            let (mut receiver, client) = pair();
            let (remote, mut sender) = pair();
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    if reverse {
                        relay(remote, client, Duration::from_millis(100))
                    } else {
                        relay(client, remote, Duration::from_millis(100))
                    }
                    .unwrap();
                });
                scope.spawn(move || {
                    for _ in 0..15 {
                        if sender.write_all(b"x").is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
                let mut bytes = Vec::new();
                receiver.read_to_end(&mut bytes).unwrap();
                assert_eq!(
                    bytes,
                    vec![b'x'; 15],
                    "A silent direction must not close an active tunnel"
                );
            });
        }
    }

    #[test]
    fn idle_and_disconnected_tunnels_release_both_directions() {
        for disconnect in [false, true] {
            let (client_peer, client) = pair();
            let (remote, mut remote_peer) = pair();
            std::thread::scope(|scope| {
                scope.spawn(move || relay(client, remote, Duration::from_millis(100)).unwrap());
                if disconnect {
                    drop(client_peer);
                }
                let mut byte = [0];
                assert_eq!(remote_peer.read(&mut byte).unwrap(), 0);
            });
        }
    }

    #[test]
    fn only_exact_tls_provider_destinations_are_accepted() {
        let allowed = vec!["api.anthropic.com".into()];
        assert!(destination("CONNECT api.anthropic.com:443 HTTP/1.1\r\n\r\n", &allowed).is_ok());
        for target in [
            "host.docker.internal:443",
            "api.anthropic.com:8787",
            "api.anthropic.com.evil.test:443",
            "user@api.anthropic.com:443",
            "127.0.0.1:443",
        ] {
            assert!(destination(&format!("CONNECT {target} HTTP/1.1\r\n\r\n"), &allowed).is_err());
        }
        assert!(destination("GET https://api.anthropic.com/ HTTP/1.1\r\n\r\n", &allowed).is_err());
        let providers = PROVIDER_HOSTS.iter().map(|host| host.to_string()).collect::<Vec<_>>();
        for host in ["chatgpt.com", "api.openai.com", "auth.openai.com", "opencode.ai"] {
            assert!(destination(&format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n"), &providers).is_ok());
        }
        for host in [
            "openai.com",
            "evil.chatgpt.com",
            "github.com",
            "www.googleapis.com",
            "play.googleapis.com",
            "unleash.codeium.com",
            "api.devin.ai",
            "server.codeium.com",
            "models.opencode.ai",
            "oauth2.googleapis.com",
            "cloudcode-pa.googleapis.com",
            "openrouter.ai",
            "generativelanguage.googleapis.com",
            "registry.npmjs.org",
            "localhost",
            "host.docker.internal",
        ] {
            assert!(destination(&format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n"), &providers).is_err());
        }
        for ip in [
            "127.0.0.1",
            "10.1.1.1",
            "172.17.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.168.1.1",
            "::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
}
