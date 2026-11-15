//! Client Redis minimal (RESP2) — sans dépendance externe (ADR-008).
//! Lecture seule par défaut ; écritures : UNLINK (suppression / purge de préfixe) et console en mode avancé.
//! Jamais `KEYS` : SCAN uniquement (F-043).
use crate::error::AppError;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const MAX_BULK: i64 = 64 * 1024 * 1024;
const VALUE_LIMIT: usize = 64 * 1024; // F-044 : valeur tronquée à 64 Ko
const ITEM_LIMIT: usize = 200;
const SCAN_MAX: usize = 500;
const DEL_MAX: usize = 1000;

#[derive(Debug, PartialEq)]
pub enum Resp {
    Simple(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Resp>),
}

impl Resp {
    pub fn text(&self) -> Option<String> {
        match self {
            Resp::Simple(s) => Some(s.clone()),
            Resp::Bulk(Some(b)) => Some(String::from_utf8_lossy(b).to_string()),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        if let Resp::Int(i) = self { Some(*i) } else { None }
    }
    pub fn array(self) -> Vec<Resp> {
        if let Resp::Array(a) = self { a } else { vec![] }
    }
}

fn io<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Io(e.to_string())
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String, AppError> {
    let mut s = String::new();
    if r.read_line(&mut s).map_err(io)? == 0 {
        return Err(AppError::Io("connexion Redis fermée".into()));
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

pub fn read_resp<R: BufRead>(r: &mut R) -> Result<Resp, AppError> {
    let line = read_line(r)?;
    let Some(&marker) = line.as_bytes().first() else {
        return Err(AppError::Io("réponse Redis vide".into()));
    };
    if !marker.is_ascii() {
        return Err(AppError::Io("réponse Redis invalide".into()));
    }
    let rest = &line[1..];
    match marker {
        b'+' => Ok(Resp::Simple(rest.to_string())),
        b'-' => Err(AppError::Io(format!("Redis : {rest}"))),
        b':' => Ok(Resp::Int(rest.parse().map_err(io)?)),
        b'$' => {
            let n: i64 = rest.parse().map_err(io)?;
            if n < 0 {
                return Ok(Resp::Bulk(None));
            }
            if n > MAX_BULK {
                return Err(AppError::Io("valeur Redis trop volumineuse".into()));
            }
            let mut buf = vec![0u8; n as usize + 2];
            r.read_exact(&mut buf).map_err(io)?;
            buf.truncate(n as usize);
            Ok(Resp::Bulk(Some(buf)))
        }
        b'*' => {
            let n: i64 = rest.parse().map_err(io)?;
            let mut v = Vec::with_capacity(n.clamp(0, 1024) as usize);
            for _ in 0..n.max(0) {
                v.push(read_resp(r)?);
            }
            Ok(Resp::Array(v))
        }
        _ => Err(AppError::Io("réponse Redis inconnue".into())),
    }
}

pub fn encode<S: AsRef<[u8]>>(args: &[S]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        let a = a.as_ref();
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Cible de connexion : port local + mot de passe optionnel (jamais journalisé).
#[derive(Clone)]
pub struct Target {
    pub port: u16,
    pub password: Option<String>,
}

impl Target {
    #[cfg(test)]
    pub fn new(port: u16) -> Self {
        Target { port, password: None }
    }
}

pub struct Conn {
    stream: BufReader<TcpStream>,
}

impl Conn {
    pub fn connect(t: &Target) -> Result<Conn, AppError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], t.port));
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
            .map_err(|_| AppError::Io(format!("Redis injoignable sur 127.0.0.1:{}", t.port)))?;
        s.set_read_timeout(Some(Duration::from_secs(5))).map_err(io)?;
        s.set_write_timeout(Some(Duration::from_secs(5))).map_err(io)?;
        let mut c = Conn { stream: BufReader::new(s) };
        if let Some(pw) = &t.password {
            c.cmd(&["AUTH", pw.as_str()])?;
        }
        Ok(c)
    }

    pub fn cmd<S: AsRef<[u8]>>(&mut self, args: &[S]) -> Result<Resp, AppError> {
        self.stream.get_mut().write_all(&encode(args)).map_err(io)?;
        read_resp(&mut self.stream)
    }
}

// ---------- INFO ----------

#[derive(Serialize, Debug, Default, PartialEq)]
pub struct RedisInfo {
    pub version: String,
    pub used_memory: u64,
    pub used_memory_human: String,
    pub connected_clients: u64,
    pub uptime_seconds: u64,
    pub ops_per_sec: u64,
    pub keyspace_hits: u64,
    pub keyspace_misses: u64,
    pub dbsize: i64,
}

pub fn parse_info(text: &str) -> RedisInfo {
    let mut i = RedisInfo::default();
    for line in text.lines() {
        let Some((k, v)) = line.trim().split_once(':') else { continue };
        let n = || v.parse::<u64>().unwrap_or(0);
        match k {
            "redis_version" => i.version = v.to_string(),
            "used_memory" => i.used_memory = n(),
            "used_memory_human" => i.used_memory_human = v.to_string(),
            "connected_clients" => i.connected_clients = n(),
            "uptime_in_seconds" => i.uptime_seconds = n(),
            "instantaneous_ops_per_sec" => i.ops_per_sec = n(),
            "keyspace_hits" => i.keyspace_hits = n(),
            "keyspace_misses" => i.keyspace_misses = n(),
            _ => {}
        }
    }
    i
}

pub fn info(t: &Target) -> Result<RedisInfo, AppError> {
    let mut c = Conn::connect(t)?;
    let text = c.cmd(&["INFO"])?.text().unwrap_or_default();
    let mut i = parse_info(&text);
    i.dbsize = c.cmd(&["DBSIZE"])?.int().unwrap_or(0);
    Ok(i)
}

// ---------- SCAN ----------

#[derive(Serialize, Debug)]
pub struct KeyMeta {
    pub key: String,
    pub kind: String,
    pub ttl: i64,
}

#[derive(Serialize, Debug)]
pub struct ScanPage {
    /// "0" = fin du parcours.
    pub cursor: String,
    pub keys: Vec<KeyMeta>,
}

fn valid_cursor(c: &str) -> bool {
    !c.is_empty() && c.len() <= 20 && c.bytes().all(|b| b.is_ascii_digit())
}

pub fn scan(t: &Target, pattern: &str, cursor: &str, count: usize) -> Result<ScanPage, AppError> {
    if !valid_cursor(cursor) {
        return Err(AppError::Io("curseur invalide".into()));
    }
    let pattern = if pattern.is_empty() { "*" } else { pattern };
    let count = count.clamp(10, SCAN_MAX).to_string();
    let mut c = Conn::connect(t)?;
    let reply = c.cmd(&["SCAN", cursor, "MATCH", pattern, "COUNT", &count])?.array();
    let mut it = reply.into_iter();
    let next = it.next().and_then(|r| r.text()).unwrap_or_else(|| "0".into());
    let raw = it.next().map(|r| r.array()).unwrap_or_default();
    let mut keys = Vec::with_capacity(raw.len());
    for k in raw {
        let Some(key) = k.text() else { continue };
        let kind = c.cmd(&["TYPE", &key])?.text().unwrap_or_default();
        let ttl = c.cmd(&["TTL", &key])?.int().unwrap_or(-1);
        keys.push(KeyMeta { key, kind, ttl });
    }
    Ok(ScanPage { cursor: next, keys })
}

// ---------- détail d'une clé ----------

#[derive(Serialize, Debug)]
pub struct KeyDetail {
    pub key: String,
    pub kind: String,
    pub ttl: i64,
    pub size: i64,
    pub value: Value,
    pub truncated: bool,
}

fn check_key(key: &str) -> Result<(), AppError> {
    if key.is_empty() || key.len() > 1024 {
        return Err(AppError::Io("nom de clé invalide".into()));
    }
    Ok(())
}

fn texts(v: Vec<Resp>) -> Vec<String> {
    v.iter().filter_map(|r| r.text()).collect()
}

fn pairs(flat: Vec<String>) -> Value {
    Value::Array(flat.chunks(2).map(|p| json!([p[0], p.get(1).cloned().unwrap_or_default()])).collect())
}

pub fn get(t: &Target, key: &str) -> Result<KeyDetail, AppError> {
    check_key(key)?;
    let mut c = Conn::connect(t)?;
    let kind = c.cmd(&["TYPE", key])?.text().unwrap_or_default();
    if kind == "none" {
        return Err(AppError::Io("clé introuvable (expirée ou supprimée)".into()));
    }
    let ttl = c.cmd(&["TTL", key])?.int().unwrap_or(-1);
    let lim = ITEM_LIMIT.to_string();
    let end = (ITEM_LIMIT - 1).to_string();
    let (size, value, truncated) = match kind.as_str() {
        "string" => {
            let size = c.cmd(&["STRLEN", key])?.int().unwrap_or(0);
            let Resp::Bulk(Some(b)) = c.cmd(&["GET", key])? else {
                return Err(AppError::Io("clé introuvable".into()));
            };
            let cut = b.len() > VALUE_LIMIT;
            (size, Value::String(String::from_utf8_lossy(&b[..b.len().min(VALUE_LIMIT)]).to_string()), cut)
        }
        "hash" => {
            let size = c.cmd(&["HLEN", key])?.int().unwrap_or(0);
            let mut r = c.cmd(&["HSCAN", key, "0", "COUNT", &lim])?.array().into_iter();
            let cur = r.next().and_then(|x| x.text()).unwrap_or_default();
            let flat = texts(r.next().map(|x| x.array()).unwrap_or_default());
            (size, pairs(flat), cur != "0")
        }
        "list" => {
            let size = c.cmd(&["LLEN", key])?.int().unwrap_or(0);
            let items = texts(c.cmd(&["LRANGE", key, "0", &end])?.array());
            (size, json!(items), size as usize > ITEM_LIMIT)
        }
        "set" => {
            let size = c.cmd(&["SCARD", key])?.int().unwrap_or(0);
            let mut r = c.cmd(&["SSCAN", key, "0", "COUNT", &lim])?.array().into_iter();
            let cur = r.next().and_then(|x| x.text()).unwrap_or_default();
            let items = texts(r.next().map(|x| x.array()).unwrap_or_default());
            (size, json!(items), cur != "0")
        }
        "zset" => {
            let size = c.cmd(&["ZCARD", key])?.int().unwrap_or(0);
            let flat = texts(c.cmd(&["ZRANGE", key, "0", &end, "WITHSCORES"])?.array());
            (size, pairs(flat), size as usize > ITEM_LIMIT)
        }
        other => (0, Value::String(format!("(type « {other} » non affiché)")), false),
    };
    Ok(KeyDetail { key: key.to_string(), kind, ttl, size, value, truncated })
}

// ---------- suppression ----------

/// UNLINK de clés explicites (confirmation côté UI). Retourne le nombre supprimé.
pub fn del(t: &Target, keys: &[String]) -> Result<i64, AppError> {
    if keys.is_empty() || keys.len() > DEL_MAX {
        return Err(AppError::Io(format!("entre 1 et {DEL_MAX} clés attendues")));
    }
    keys.iter().try_for_each(|k| check_key(k))?;
    let mut c = Conn::connect(t)?;
    let mut args: Vec<&str> = vec!["UNLINK"];
    args.extend(keys.iter().map(|k| k.as_str()));
    Ok(c.cmd(&args)?.int().unwrap_or(0))
}

/// Échappe les métacaractères glob de MATCH.
pub fn glob_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            o.push('\\');
        }
        o.push(ch);
    }
    o
}

/// Purge d'un préfixe par SCAN + UNLINK par lots (même logique que `clear()` d'astral-cache :
/// jamais FLUSHDB). Un préfixe vide est refusé.
pub fn flush_prefix(t: &Target, prefix: &str) -> Result<u64, AppError> {
    if prefix.trim().is_empty() {
        return Err(AppError::Io("préfixe vide refusé (utilisez un préfixe explicite)".into()));
    }
    check_key(prefix)?;
    let pattern = format!("{}*", glob_escape(prefix));
    let mut c = Conn::connect(t)?;
    let mut cursor = "0".to_string();
    let mut total = 0u64;
    loop {
        let reply = c.cmd(&["SCAN", &cursor, "MATCH", &pattern, "COUNT", "500"])?.array();
        let mut it = reply.into_iter();
        cursor = it.next().and_then(|r| r.text()).unwrap_or_else(|| "0".into());
        let keys = texts(it.next().map(|r| r.array()).unwrap_or_default());
        if !keys.is_empty() {
            let mut args: Vec<&str> = vec!["UNLINK"];
            args.extend(keys.iter().map(|k| k.as_str()));
            total += c.cmd(&args)?.int().unwrap_or(0).max(0) as u64;
        }
        if cursor == "0" {
            return Ok(total);
        }
    }
}

// ---------- console (F-049) ----------

/// Commandes de lecture autorisées hors mode avancé.
const READ_CMDS: &[&str] = &[
    "GET", "MGET", "EXISTS", "TYPE", "TTL", "PTTL", "EXPIRETIME", "PEXPIRETIME", "STRLEN", "HGET", "HMGET",
    "HGETALL", "HLEN", "HEXISTS", "HKEYS", "HVALS", "LRANGE", "LLEN", "LINDEX", "SMEMBERS", "SCARD",
    "SISMEMBER", "ZRANGE", "ZCARD", "ZSCORE", "ZRANK", "SCAN", "SSCAN", "HSCAN", "ZSCAN", "INFO", "DBSIZE",
    "PING", "ECHO", "TIME", "RANDOMKEY", "OBJECT",
];

/// Jamais exécutées : incompatibles avec le mode requête/réponse ou contraires à la règle « SCAN, jamais KEYS ».
const NEVER_CMDS: &[&str] = &[
    "KEYS", "MONITOR", "SUBSCRIBE", "PSUBSCRIBE", "SSUBSCRIBE", "SYNC", "PSYNC", "BLPOP", "BRPOP", "BRPOPLPUSH",
    "BLMOVE", "BLMPOP", "BZPOPMIN", "BZPOPMAX", "WAIT", "MULTI", "EXEC", "DISCARD", "WATCH", "AUTH", "HELLO",
    "CLIENT", "RESET", "QUIT",
];

#[derive(Debug, PartialEq)]
pub enum Access {
    Read,
    Advanced,
    Never,
}

pub fn classify(cmd: &str) -> Access {
    let c = cmd.to_ascii_uppercase();
    if NEVER_CMDS.contains(&c.as_str()) {
        Access::Never
    } else if READ_CMDS.contains(&c.as_str()) {
        Access::Read
    } else {
        Access::Advanced
    }
}

/// Découpe une ligne de commande : espaces, guillemets doubles (échappements \" et \\).
pub fn parse_line(line: &str) -> Result<Vec<String>, AppError> {
    if line.len() > 4096 {
        return Err(AppError::Io("commande trop longue".into()));
    }
    let mut args = vec![];
    let mut cur = String::new();
    let (mut in_q, mut has) = (false, false);
    let mut it = line.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            '\\' if in_q => match it.next() {
                Some(n @ ('"' | '\\')) => cur.push(n),
                Some(n) => {
                    cur.push('\\');
                    cur.push(n);
                }
                None => return Err(AppError::Io("échappement incomplet".into())),
            },
            c if c.is_whitespace() && !in_q => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if in_q {
        return Err(AppError::Io("guillemet non fermé".into()));
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    if args.is_empty() || args.len() > 64 {
        return Err(AppError::Io("entre 1 et 64 éléments attendus".into()));
    }
    Ok(args)
}

pub fn format_resp(r: &Resp, depth: usize) -> String {
    let pad = "   ".repeat(depth);
    match r {
        Resp::Simple(s) => s.clone(),
        Resp::Int(i) => format!("(integer) {i}"),
        Resp::Bulk(None) => "(nil)".into(),
        Resp::Bulk(Some(b)) => {
            let cut = b.len() > VALUE_LIMIT;
            let s = String::from_utf8_lossy(&b[..b.len().min(VALUE_LIMIT)]);
            format!("\"{s}\"{}", if cut { " … (tronqué)" } else { "" })
        }
        Resp::Array(a) if a.is_empty() => "(empty array)".into(),
        Resp::Array(a) => a
            .iter()
            .enumerate()
            .take(500)
            .map(|(i, x)| format!("{pad}{}) {}", i + 1, format_resp(x, depth + 1)))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Exécute une commande saisie dans la console. Hors `advanced` : lecture seule (liste blanche).
pub fn exec(t: &Target, line: &str, advanced: bool) -> Result<String, AppError> {
    let args = parse_line(line)?;
    let name = args[0].to_ascii_uppercase();
    match classify(&name) {
        Access::Never => {
            return Err(AppError::NotStartable(format!(
                "{name} est refusée (KEYS → utilisez SCAN ; les commandes bloquantes/abonnements ne sont pas gérées)"
            )))
        }
        Access::Advanced if !advanced => {
            return Err(AppError::NotStartable(format!(
                "{name} n'est pas une commande de lecture : activez le mode avancé"
            )))
        }
        _ => {}
    }
    let mut c = Conn::connect(t)?;
    Ok(format_resp(&c.cmd(&args)?, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::paths::AppPaths;
    use crate::services;
    use crate::supervisor::Supervisor;
    use std::io::Cursor;

    fn parse(bytes: &[u8]) -> Result<Resp, AppError> {
        read_resp(&mut Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn resp_parsing() {
        assert_eq!(parse(b"+PONG\r\n").unwrap(), Resp::Simple("PONG".into()));
        assert_eq!(parse(b":42\r\n").unwrap(), Resp::Int(42));
        assert_eq!(parse(b"$3\r\nfoo\r\n").unwrap(), Resp::Bulk(Some(b"foo".to_vec())));
        assert_eq!(parse(b"$-1\r\n").unwrap(), Resp::Bulk(None));
        assert_eq!(
            parse(b"*2\r\n$1\r\n0\r\n*1\r\n$1\r\na\r\n").unwrap(),
            Resp::Array(vec![Resp::Bulk(Some(b"0".to_vec())), Resp::Array(vec![Resp::Bulk(Some(b"a".to_vec()))])])
        );
        assert!(parse(b"-ERR boom\r\n").is_err());
        assert!(parse(b"").is_err());
        assert!(parse(b"\xc3\xa9\r\n").is_err());
    }

    #[test]
    fn encodes_commands() {
        assert_eq!(encode(&["GET", "k"]), b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n".to_vec());
    }

    #[test]
    fn info_parsing() {
        let i = parse_info("# Server\r\nredis_version:5.0.14\r\nused_memory:1024\r\nused_memory_human:1.00K\r\nconnected_clients:3\r\nkeyspace_hits:9\r\nkeyspace_misses:1\r\n");
        assert_eq!(i.version, "5.0.14");
        assert_eq!(i.used_memory, 1024);
        assert_eq!(i.connected_clients, 3);
        assert_eq!(i.keyspace_hits, 9);
    }

    #[test]
    fn glob_escape_and_guards() {
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\");
        assert!(flush_prefix(&Target::new(1), "  ").is_err()); // refus avant toute connexion
        assert!(scan(&Target::new(1), "*", "1;rm", 10).is_err());
        assert!(del(&Target::new(1), &[]).is_err());
    }

    #[test]
    fn console_line_parsing() {
        assert_eq!(parse_line("GET  foo").unwrap(), ["GET", "foo"]);
        assert_eq!(parse_line(r#"SET k "a b" """#).unwrap(), ["SET", "k", "a b", ""]);
        assert_eq!(parse_line(r#"SET k "say \"hi\"""#).unwrap(), ["SET", "k", "say \"hi\""]);
        assert!(parse_line("").is_err());
        assert!(parse_line("GET \"x").is_err());
        assert!(parse_line(&"a ".repeat(70)).is_err());
    }

    #[test]
    fn console_classification() {
        assert_eq!(classify("get"), Access::Read);
        assert_eq!(classify("HGETALL"), Access::Read);
        for d in ["FLUSHALL", "flushdb", "CONFIG", "SHUTDOWN", "DEBUG", "SCRIPT", "EVAL", "SET", "DEL"] {
            assert_eq!(classify(d), Access::Advanced, "{d}");
        }
        for n in ["KEYS", "monitor", "SUBSCRIBE", "BLPOP", "AUTH", "CLIENT"] {
            assert_eq!(classify(n), Access::Never, "{n}");
        }
        // refus avant toute connexion
        let t = Target::new(1);
        assert!(matches!(exec(&t, "KEYS *", true), Err(AppError::NotStartable(_))));
        assert!(matches!(exec(&t, "FLUSHALL", false), Err(AppError::NotStartable(_))));
    }

    #[test]
    fn response_formatting() {
        let r = Resp::Array(vec![Resp::Bulk(Some(b"a".to_vec())), Resp::Int(2), Resp::Bulk(None)]);
        assert_eq!(format_resp(&r, 0), "1) \"a\"\n2) (integer) 2\n3) (nil)");
        assert_eq!(format_resp(&Resp::Array(vec![]), 0), "(empty array)");
    }

    /// Serveur Redis réel (port 16380) : `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn explorer_against_real_redis() {
        let mut cfg = Config::default();
        cfg.redis.port = 16380;
        cfg.paths.data = std::env::temp_dir().join(format!("astral-dev-rx-{}", std::process::id())).to_string_lossy().to_string();
        let paths = AppPaths::from_config(&cfg);
        let sup = Supervisor::new(services::build(&cfg, &paths), paths.logs());
        sup.start("redis").expect("start redis");
        let p = 16380;
        let tp = Target::new(p);
        let mut c = Conn::connect(&tp).unwrap();

        // 100 000 clés (F-043) par lots MSET
        for b in 0..100 {
            let mut a: Vec<String> = vec!["MSET".into()];
            for i in 0..1000 {
                a.push(format!("astral:cache:{}", b * 1000 + i));
                a.push("v".into());
            }
            c.cmd(&a).unwrap();
        }
        c.cmd(&["SET", "other:keep", "1"]).unwrap();
        c.cmd(&["HSET", "h:1", "a", "1", "b", "2"]).unwrap();
        c.cmd(&["RPUSH", "l:1", "x", "y"]).unwrap();
        c.cmd(&["SADD", "s:1", "m"]).unwrap();
        c.cmd(&["ZADD", "z:1", "1.5", "m"]).unwrap();
        c.cmd(&["SET", "big", &"x".repeat(100_000)]).unwrap();
        c.cmd(&["EXPIRE", "other:keep", "300"]).unwrap();

        let i = info(&tp).unwrap();
        assert!(i.dbsize >= 100_006 && i.version.starts_with('5'));

        // pagination complète sans bloquer
        let mut cur = "0".to_string();
        let mut seen = 0usize;
        loop {
            let page = scan(&tp, "astral:cache:*", &cur, 500).unwrap();
            seen += page.keys.len();
            cur = page.cursor;
            if cur == "0" {
                break;
            }
        }
        assert_eq!(seen, 100_000);

        let d = get(&tp, "h:1").unwrap();
        assert_eq!((d.kind.as_str(), d.size), ("hash", 2));
        assert_eq!(get(&tp, "l:1").unwrap().value, json!(["x", "y"]));
        assert_eq!(get(&tp, "s:1").unwrap().value, json!(["m"]));
        assert_eq!(get(&tp, "z:1").unwrap().value, json!([["m", "1.5"]]));
        let big = get(&tp, "big").unwrap();
        assert!(big.truncated && big.size == 100_000 && big.value.as_str().unwrap().len() == VALUE_LIMIT);
        assert!(get(&tp, "other:keep").unwrap().ttl > 0);
        assert!(get(&tp, "absent").is_err());

        // console (F-049) : lecture libre, écriture seulement en mode avancé, KEYS jamais
        assert_eq!(exec(&tp, "GET other:keep", false).unwrap(), "\"1\"");
        assert!(exec(&tp, "SET x 1", false).is_err());
        assert_eq!(exec(&tp, "SET x 1", true).unwrap(), "OK");
        assert!(exec(&tp, "KEYS *", true).is_err());
        assert_eq!(exec(&tp, "DEL x", true).unwrap(), "(integer) 1");

        // purge de préfixe : seules les 100 000 clés partent
        assert_eq!(flush_prefix(&tp, "astral:cache:").unwrap(), 100_000);
        assert_eq!(info(&tp).unwrap().dbsize, 6);
        assert_eq!(del(&tp, &["other:keep".into(), "nope".into()]).unwrap(), 1);

        sup.stop("redis").unwrap();
    }

    /// F-050 : Redis avec mot de passe généré (port 16382). `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn generated_password_flow() {
        let mut cfg = Config::default();
        cfg.redis.port = 16382;
        cfg.redis.password = "generated".into();
        cfg.paths.data = std::env::temp_dir().join(format!("astral-dev-pw-{}", std::process::id())).to_string_lossy().to_string();
        let paths = AppPaths::from_config(&cfg);
        let pw = services::redis_secret(&cfg, &paths).expect("secret");
        let sup = Supervisor::new(services::build(&cfg, &paths), paths.logs());
        sup.start("redis").expect("start (PING répond -NOAUTH = prêt)");

        assert!(info(&Target::new(16382)).is_err(), "sans mot de passe : refusé");
        let good = Target { port: 16382, password: Some(pw.clone()) };
        assert!(info(&good).is_ok());
        let bad = Target { port: 16382, password: Some("faux".into()) };
        assert!(info(&bad).is_err());
        let conf = std::fs::read_to_string(paths.redis_data().join("redis.conf")).unwrap();
        assert!(conf.contains(&format!("requirepass {pw}")));
        let log = std::fs::read_to_string(paths.logs().join("redis.log")).unwrap_or_default();
        assert!(!log.contains(&pw), "le mot de passe ne doit pas apparaître dans les logs");

        sup.stop("redis").expect("arrêt gracieux avec AUTH");
        assert!(!crate::probe::port_open(16382));
    }
}
