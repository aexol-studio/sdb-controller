use std::sync::Arc;
use tokio::{join, sync::RwLock};

use chrono::{DateTime, Utc};
use kube::{
    Client,
    runtime::events::{Recorder, Reporter},
};
use serde::Serialize;

use crate::{Metrics, run_cluster, run_database};

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Metrics
    pub metrics: Arc<Metrics>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Metrics getter
    pub fn metrics(&self) -> String {
        let mut buffer = String::new();
        let registry = &*self.metrics.registry;
        prometheus_client::encoding::text::encode(&mut buffer, registry).unwrap();
        buffer
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "sdb-controller".into(),
        }
    }
}
impl Diagnostics {
    pub fn recorder(&self, client: Client) -> Recorder {
        Recorder::new(client, self.reporter.clone())
    }
}

pub async fn run(state: State) {
    let client = Client::try_default().await.expect("failed to create kube Client");
    join!(
        run_cluster(state.clone(), client.clone()),
        run_database(state.clone(), client.clone()),
    );
}
