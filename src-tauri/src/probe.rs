//! Sondes TCP (et Redis PING) + commandes RESP minimales — sans dépendance externe.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&addr(port), Duration::from_millis(150)).is_ok()
}

fn redis_inline(port: u16, cmd: &str) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&addr(port), Duration::from_millis(300)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(500))).ok()?;
    s.write_all(format!("{cmd}\r\n").as_bytes()).ok()?;
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf[..n]).to_string())
}

/// Prêt = réponse `+PONG` (ou `-NOAUTH`, le serveur répond donc bien).
pub fn redis_ping(port: u16) -> bool {
    redis_inline(port, "PING")
        .map(|r| r.starts_with("+PONG") || r.starts_with("-NOAUTH"))
        .unwrap_or(false)
}

/// Arrêt gracieux. Redis coupe la connexion sans répondre : on ignore le résultat.
pub fn redis_shutdown(port: u16, save: bool, password: Option<&str>) {
    let cmd = if save { "SHUTDOWN SAVE" } else { "SHUTDOWN NOSAVE" };
    // Inline : AUTH et SHUTDOWN dans la même écriture (mot de passe hexadécimal, sans espace).
    let line = match password {
        Some(pw) => format!("AUTH {pw}\r\n{cmd}"),
        None => cmd.to_string(),
    };
    let _ = redis_inline(port, &line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn detects_open_and_closed_ports() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(port_open(p));
        drop(l);
        assert!(!port_open(p));
    }

    #[test]
    fn ping_reads_pong() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 16];
            let _ = s.read(&mut b);
            s.write_all(b"+PONG\r\n").unwrap();
        });
        assert!(redis_ping(p));
        h.join().unwrap();
    }
}