//! Superviseur : possède les processus (ADR-002). États, démarrage avec sonde, arrêt gracieux puis kill.
use crate::error::AppError;
use crate::probe;
use crate::services::{ServiceSpec, Stop, START_ORDER};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const STOP_GRACE: Duration = Duration::from_secs(5);

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct ServiceStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub meta: String,
    /// stopped | starting | running | stopping | failed | disabled
    pub state: &'static str,
    /// Lancé par astral-dev (arrêtable ici).
    pub managed: bool,
    /// Port occupé par un processus que nous n'avons pas lancé (ex. Laragon).
    pub external: bool,
    pub runtime: bool,
    pub pid: Option<u32>,
    pub error: Option<String>,
    /// Mode autonome : port occupé par un programme étranger (ex. Laragon) → message d'alerte.
    pub conflict: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct OpResult {
    pub id: &'static str,
    pub ok: bool,
    pub note: Option<String>,
}

#[derive(Default)]
struct Inner {
    children: HashMap<&'static str, Child>,
    transient: HashMap<&'static str, &'static str>,
    errors: HashMap<&'static str, String>,
}

pub struct Supervisor {
    specs: Vec<ServiceSpec>,
    logs: PathBuf,
    inner: Mutex<Inner>,
    /// Racine d'astral-dev (mode autonome) : un port occupé hors de cette racine est un conflit.
    own_root: Option<String>,
    owners: Mutex<HashMap<&'static str, Option<String>>>,
}

fn hide(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn kill_tree(child: &mut Child) {
    let mut c = Command::new("taskkill");
    hide(&mut c)
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = c.status();
    let _ = child.kill();
    let _ = child.wait();
}

impl Supervisor {
    pub fn new(specs: Vec<ServiceSpec>, logs: PathBuf) -> Self {
        Self { specs, logs, inner: Mutex::new(Inner::default()), own_root: None, owners: Mutex::new(HashMap::new()) }
    }

    pub fn with_own_root(mut self, root: Option<&str>) -> Self {
        self.own_root = root.map(|r| r.replace('\\', "/").to_lowercase());
        self
    }

    /// Alerte si le service est « externe » et que son processus n'appartient pas à astral-dev.
    fn conflict_of(&self, spec: &ServiceSpec, external: bool) -> Option<String> {
        let root = self.own_root.as_ref()?;
        let port = spec.port?;
        if !external {
            self.owners.lock().unwrap_or_else(|e| e.into_inner()).remove(spec.id);
            return None;
        }
        let cached = self.owners.lock().unwrap_or_else(|e| e.into_inner()).get(spec.id).cloned();
        let owner = match cached {
            Some(o) => o,
            None => {
                let o = probe::port_owner(port);
                self.owners.lock().unwrap_or_else(|e| e.into_inner()).insert(spec.id, o.clone());
                o
            }
        };
        let path = owner?;
        if path.replace('\\', "/").to_lowercase().starts_with(root.as_str()) {
            return None;
        }
        Some(format!("Port {port} occupé par un autre programme : {path}. Changez le port dans astral-dev.toml ou arrêtez ce programme."))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn spec(&self, id: &str) -> Result<&ServiceSpec, AppError> {
        self.specs.iter().find(|s| s.id == id).ok_or_else(|| AppError::UnknownService(id.into()))
    }

    pub fn log_path(&self, id: &str) -> PathBuf {
        self.logs.join(format!("{id}.log"))
    }

    // ---------- état ----------

    pub fn statuses(&self) -> Vec<ServiceStatus> {
        // Sondes en parallèle : un port fermé coûte ~150 ms.
        let open: Vec<bool> = std::thread::scope(|sc| {
            let hs: Vec<_> = self
                .specs
                .iter()
                .map(|s| sc.spawn(move || s.port.map(probe::port_open).unwrap_or(false)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap_or(false)).collect()
        });
        self.specs
            .iter()
            .zip(open)
            .map(|(s, o)| {
                let mut st = self.status_of(s, o);
                st.conflict = self.conflict_of(s, st.external);
                st
            })
            .collect()
    }

    fn status_of(&self, spec: &ServiceSpec, open: bool) -> ServiceStatus {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let mut pid = None;
        let mut exited = None;
        if let Some(ch) = inner.children.get_mut(spec.id) {
            match ch.try_wait() {
                Ok(Some(st)) => exited = Some(format!("processus terminé ({st})")),
                Ok(None) => pid = Some(ch.id()),
                Err(_) => {}
            }
        }
        if let Some(msg) = exited {
            inner.children.remove(spec.id);
            inner.errors.insert(spec.id, msg);
        }
        let managed = inner.children.contains_key(spec.id);
        let transient = inner.transient.get(spec.id).copied();
        let error = inner.errors.get(spec.id).cloned();
        drop(guard);

        let state = if !spec.enabled {
            "disabled"
        } else if let Some(t) = transient {
            t
        } else if spec.runtime {
            if spec.launch.is_ok() { "running" } else { "failed" }
        } else if open {
            "running"
        } else if error.is_some() {
            "failed"
        } else {
            "stopped"
        };
        let error = error.or_else(|| {
            if spec.runtime { spec.launch.as_ref().err().cloned() } else { None }
        });
        ServiceStatus {
            id: spec.id,
            name: spec.name,
            meta: spec.meta.clone(),
            state,
            managed,
            external: open && !managed && !spec.runtime,
            runtime: spec.runtime,
            pid,
            error,
            conflict: None,
        }
    }

    // ---------- démarrage ----------

    pub fn start(&self, id: &str) -> Result<(), AppError> {
        let spec = self.spec(id)?;
        if spec.runtime {
            return Err(AppError::NotStartable(format!("{} n'a pas de processus propre", spec.name)));
        }
        if !spec.enabled {
            return Err(AppError::NotStartable(format!("{} est désactivé dans la configuration", spec.name)));
        }
        if self.lock().children.contains_key(spec.id) {
            return Ok(());
        }
        let port = spec.port.unwrap_or(0);
        if probe::port_open(port) {
            return Err(AppError::PortInUse(port)); // F-004
        }
        let launch = spec.launch.as_ref().map_err(|e| AppError::BinaryNotFound(e.clone()))?;

        {
            let mut g = self.lock();
            g.transient.insert(spec.id, "starting");
            g.errors.remove(spec.id);
        }
        let res = self.do_start(spec, launch, port);
        let mut g = self.lock();
        g.transient.remove(spec.id);
        if let Err(e) = &res {
            g.errors.insert(spec.id, e.to_string());
        }
        res
    }

    fn ready(spec: &ServiceSpec, port: u16) -> bool {
        if spec.id == "redis" { probe::redis_ping(port) } else { probe::port_open(port) }
    }

    fn do_start(&self, spec: &ServiceSpec, launch: &crate::services::Launch, port: u16) -> Result<(), AppError> {
        fs::create_dir_all(&self.logs)?;
        for (path, content) in &launch.write_files {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(path, content)?;
            if spec.id == "redis" && path.file_name().is_some_and(|n| n == "redis.conf") {
                crate::secrets::restrict_to_user(path); // contient requirepass
            }
        }
        let log_path = self.log_path(spec.id);
        if let Some(init) = &launch.init {
            if !init.marker.exists() {
                let out = OpenOptions::new().create(true).append(true).open(&log_path)?;
                let err = out.try_clone()?;
                let mut c = Command::new(&init.program);
                c.args(&init.args).current_dir(&init.cwd).stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err));
                hide(&mut c);
                let st = c
                    .status()
                    .map_err(|e| AppError::StartFailed(format!("{} : initialisation impossible ({e})", spec.name)))?;
                if !st.success() {
                    return Err(AppError::StartFailed(format!("{} : initialisation échouée ({st}), voir les logs", spec.name)));
                }
            }
        }
        let out = OpenOptions::new().create(true).append(true).open(&log_path)?;
        let err = out.try_clone()?;
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args)
            .current_dir(&launch.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        hide(&mut cmd);
        let child = cmd
            .spawn()
            .map_err(|e| AppError::StartFailed(format!("{} : lancement impossible ({e})", spec.name)))?;
        self.lock().children.insert(spec.id, child);

        let timeout = Duration::from_secs(spec.start_timeout_secs);
        let deadline = Instant::now() + timeout;
        loop {
            let exited = {
                let mut g = self.lock();
                let status = match g.children.get_mut(spec.id) {
                    Some(c) => c.try_wait().ok().flatten().map(|s| s.to_string()),
                    None => Some("processus absent".into()),
                };
                if status.is_some() {
                    g.children.remove(spec.id);
                }
                status
            };
            if let Some(st) = exited {
                return Err(AppError::StartFailed(format!(
                    "{} s'est arrêté au démarrage ({st}) — voir {}",
                    spec.name,
                    log_path.display()
                )));
            }
            if Self::ready(spec, port) {
                return Ok(());
            }
            if Instant::now() > deadline {
                self.kill(spec.id);
                return Err(AppError::Timeout(format!("{} n'a pas répondu en {} s", spec.name, timeout.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // ---------- arrêt ----------

    fn kill(&self, id: &str) {
        let child = self.lock().children.remove(id);
        if let Some(mut c) = child {
            kill_tree(&mut c);
        }
    }

    pub fn stop(&self, id: &str) -> Result<(), AppError> {
        let spec = self.spec(id)?;
        if spec.runtime {
            return Err(AppError::NotStartable(format!("{} n'a pas de processus propre", spec.name)));
        }
        if !self.lock().children.contains_key(spec.id) {
            return match spec.port {
                Some(p) if probe::port_open(p) => Err(AppError::NotManaged(spec.name.into())),
                _ => Ok(()),
            };
        }
        self.lock().transient.insert(spec.id, "stopping");

        match &spec.stop {
            Stop::Kill => {}
            Stop::Redis { port, save, password } => probe::redis_shutdown(*port, *save, password.as_deref()),
            Stop::MysqlAdmin { exe, port } => {
                if exe.is_file() {
                    let mut c = Command::new(exe);
                    hide(&mut c)
                        .args(["-uroot", "--protocol=tcp", "--host=127.0.0.1", &format!("--port={port}"), "shutdown"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());
                    let _ = c.status();
                }
            }
        }
        // Les services « Kill » n'ont pas d'arrêt gracieux : on passe directement à la fin d'arbre.
        if !matches!(spec.stop, Stop::Kill) {
            let deadline = Instant::now() + STOP_GRACE;
            while Instant::now() < deadline {
                let gone = {
                    let mut g = self.lock();
                    match g.children.get_mut(spec.id) {
                        Some(c) => c.try_wait().ok().flatten().is_some(),
                        None => true,
                    }
                };
                if gone {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        self.kill(spec.id); // no-op si déjà terminé (taskkill échoue silencieusement)
        let mut g = self.lock();
        g.transient.remove(spec.id);
        g.errors.remove(spec.id);
        Ok(())
    }

    pub fn restart(&self, id: &str) -> Result<(), AppError> {
        self.stop(id)?;
        self.start(id)
    }

    // ---------- groupes ----------

    pub fn start_all(&self) -> Vec<OpResult> {
        let mut out = vec![];
        for id in START_ORDER {
            let Ok(spec) = self.spec(id) else { continue };
            if !spec.enabled {
                continue;
            }
            let external = spec.port.map(probe::port_open).unwrap_or(false) && !self.lock().children.contains_key(id);
            if external {
                out.push(OpResult { id, ok: true, note: Some("déjà actif (externe)".into()) });
                continue;
            }
            match self.start(id) {
                Ok(()) => out.push(OpResult { id, ok: true, note: None }),
                Err(e) => out.push(OpResult { id, ok: false, note: Some(e.to_string()) }),
            }
        }
        out
    }

    /// N'arrête que les services lancés par astral-dev.
    pub fn stop_all(&self) -> Vec<OpResult> {
        let mut out = vec![];
        for id in START_ORDER.iter().rev().copied() {
            if !self.lock().children.contains_key(id) {
                continue;
            }
            match self.stop(id) {
                Ok(()) => out.push(OpResult { id, ok: true, note: None }),
                Err(e) => out.push(OpResult { id, ok: false, note: Some(e.to_string()) }),
            }
        }
        out
    }

    // ---------- logs ----------

    pub fn logs(&self, id: &str, lines: usize) -> Result<Vec<String>, AppError> {
        self.spec(id)?;
        let path = self.log_path(id);
        let Ok(mut f) = fs::File::open(&path) else { return Ok(vec![]) };
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(256 * 1024)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf);
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        Ok(all[start..].iter().map(|s| s.to_string()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::paths::AppPaths;
    use crate::services;

    fn sup_with(cfg: &Config, tag: &str) -> Supervisor {
        let mut cfg = cfg.clone();
        cfg.paths.data = std::env::temp_dir().join(format!("astral-dev-{tag}-{}", std::process::id())).to_string_lossy().to_string();
        let paths = AppPaths::from_config(&cfg);
        Supervisor::new(services::build(&cfg, &paths), paths.logs())
    }

    #[test]
    fn unknown_service_and_runtime_are_rejected() {
        let s = sup_with(&Config::default(), "rej");
        assert!(matches!(s.start("nope"), Err(AppError::UnknownService(_))));
        assert!(matches!(s.start("php"), Err(AppError::NotStartable(_))));
    }

    #[test]
    fn occupied_port_is_refused_and_flagged_external() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = Config::default();
        cfg.redis.port = l.local_addr().unwrap().port();
        let s = sup_with(&cfg, "port");
        assert!(matches!(s.start("redis"), Err(AppError::PortInUse(_))));
        let st = s.statuses().into_iter().find(|x| x.id == "redis").unwrap();
        assert!(st.external && st.state == "running" && !st.managed);
        assert!(matches!(s.stop("redis"), Err(AppError::NotManaged(_))));
    }

    /// Pile complète sur les ports par défaut : Laragon doit être arrêté.
    #[test]
    #[ignore]
    fn full_stack_cycle() {
        let s = sup_with(&Config::default(), "full");
        let res = s.start_all();
        for r in &res {
            assert!(r.ok, "{} : {:?}", r.id, r.note);
        }
        for st in s.statuses().into_iter().filter(|x| !x.runtime) {
            assert!(st.state == "running" && st.managed, "{} : {} {:?}", st.id, st.state, st.error);
        }
        let stopped = s.stop_all();
        assert!(stopped.iter().all(|r| r.ok), "{stopped:?}");
        for st in s.statuses().into_iter().filter(|x| !x.runtime) {
            assert_eq!(st.state, "stopped", "{}", st.id);
        }
    }

    /// Nécessite le vrai redis-server.exe : `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn redis_full_cycle() {
        let mut cfg = Config::default();
        cfg.redis.port = 16379;
        let s = sup_with(&cfg, "redis");
        s.start("redis").expect("start");
        let st = s.statuses().into_iter().find(|x| x.id == "redis").unwrap();
        assert!(st.state == "running" && st.managed && st.pid.is_some());
        assert!(probe::redis_ping(16379));
        s.stop("redis").expect("stop");
        assert!(!probe::port_open(16379));
        let st = s.statuses().into_iter().find(|x| x.id == "redis").unwrap();
        assert_eq!(st.state, "stopped");
    }
}