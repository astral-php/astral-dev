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

/// Chemin de l'exécutable qui écoute sur ce port (127.0.0.1 / toutes interfaces). Meilleur effort.
pub fn port_owner(port: u16) -> Option<String> {
    use std::process::Command;
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("netstat");
    c.args(["-ano", "-p", "TCP"]);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    let out = c.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let pid = parse_netstat_pid(&text, port)?;
    let mut p = Command::new("powershell");
    p.args(["-NoProfile", "-NonInteractive", "-Command", &format!("(Get-Process -Id {pid}).Path")]);
    #[cfg(windows)]
    p.creation_flags(0x0800_0000);
    let o = p.output().ok()?;
    let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// PID du processus en écoute sur `port` d'après la sortie de `netstat -ano`.
pub fn parse_netstat_pid(text: &str, port: u16) -> Option<u32> {
    let suffix = format!(":{port}");
    text.lines().find_map(|l| {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() >= 5 && cols[2].ends_with(":0") && cols[1].ends_with(&suffix) {
            cols[4].parse().ok()
        } else {
            None
        }
    })
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

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn finds_listening_pid() {
        let t = "  TCP    127.0.0.1:8080   0.0.0.0:0   LISTENING   4242
  TCP    0.0.0.0:80   0.0.0.0:0   LISTENING   99
  TCP    127.0.0.1:18080  0.0.0.0:0  LISTENING  7
";
        assert_eq!(parse_netstat_pid(t, 8080), Some(4242));
        assert_eq!(parse_netstat_pid(t, 80), Some(99));
        assert_eq!(parse_netstat_pid(t, 9), None);
    }
}
