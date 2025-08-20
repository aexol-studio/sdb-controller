use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),

    #[error("Unknown error: {0}")]
    StdError(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("HTTP Error: {0}")]
    HTTPError(#[source] http::Error),

    #[error("Retryable Kube Error: {0}")]
    RetryableKubeError(#[source] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("MissingRootCredentials")]
    MissingRootCredentials,

    #[error("service request error: {0}")]
    ServiceRequestError(String),

    #[error("reqwest error: {0}")]
    ReqwestError(#[source] reqwest::Error),

    #[error("url parse error: {0}")]
    URLParseError(#[source] url::ParseError),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

/// Expose all cluster components used by main
pub mod cluster;
pub use crate::cluster::crd::Cluster;

/// Expose all database components used by main
pub mod database;
pub use crate::database::crd::Database;

/// Expose all user components used by main
pub mod user;
pub use crate::user::crd::User;

/// Log and trace integrations
pub mod telemetry;

/// Run all controllers
pub mod run;
pub use run::*;

pub(crate) mod service;

/// Metrics
mod metrics;
pub use metrics::Metrics;
