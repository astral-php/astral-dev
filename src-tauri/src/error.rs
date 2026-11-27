use serde::Serialize;

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "code", content = "message")]
pub enum AppError {
    #[error("port déjà utilisé : {0}")]
    PortInUse(u16),
    #[error("binaire introuvable : {0}")]
    BinaryNotFound(String),
    #[error("configuration invalide : {0}")]
    Config(String),
    #[error("service inconnu : {0}")]
    UnknownService(String),
    #[error("service externe, non géré par Rustral Studio : {0}")]
    NotManaged(String),
    #[error("action impossible : {0}")]
    NotStartable(String),
    #[error("{0}")]
    StartFailed(String),
    #[error("délai dépassé : {0}")]
    Timeout(String),
    #[error("erreur E/S : {0}")]
    Io(String),
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e.to_string())
    }
}