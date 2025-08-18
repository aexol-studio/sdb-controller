use crate::{Cluster, Error, Result, run::State};
use base64::Engine;
use futures::StreamExt;
use http::{HeaderValue, Method, Request};
use kube::{
    CustomResource, Resource,
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::Recorder,
        finalizer::{Event as Finalizer, finalizer},
        watcher::Config,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tracing::*;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Database",
    group = "surrealdb.aexol.com",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "DbStatus", shortname = "db")]
#[serde(rename_all = "camelCase")]
pub struct DbSpec {
    pub cluster_ref: ClusterRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_name: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DbStatus {
    #[serde(default)]
    pub phase: Option<DbPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum DbPhase {
    #[default]
    New,
    InProgress,
    Ready,
    Error,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Clone)]
pub struct DbContext {
    pub client: Client,
    pub recorder: Recorder,
}

#[instrument(skip(ctx, db), fields(trace_id))]
async fn reconcile(db: Arc<Database>, ctx: Arc<DbContext>) -> Result<Action> {
    let ns = db.namespace().unwrap();
    let dbs: Api<Database> = Api::namespaced(ctx.client.clone(), &ns);
    info!("Reconciling Database \"{}\" in {}", db.name_any(), ns);
    // drive a finalizer for potential future cleanup (we currently don't delete logical NS/DB)
    finalizer(&dbs, "databases.surrealdb.aexol.com", db, |event| async {
        match event {
            Finalizer::Apply(db) => db.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(db) => db.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn error_policy(_db: Arc<Database>, _err: &Error, _ctx: Arc<DbContext>) -> Action {
    Action::requeue(Duration::from_secs(60))
}

impl Database {
    async fn set_phase_msg(
        &self,
        ctx: Arc<DbContext>,
        phase: DbPhase,
        message: Option<String>,
    ) -> Result<()> {
        let ns = self.namespace().unwrap();
        let api: Api<Database> = Api::namespaced(ctx.client.clone(), &ns);
        #[derive(serde::Serialize, Debug)]
        struct PhaseOnly<'a> {
            phase: &'a DbPhase,
            #[serde(skip_serializing_if = "Option::is_none")]
            message: Option<&'a String>,
        }
        #[derive(serde::Serialize, Debug)]
        struct StatusPatch<'a> {
            status: PhaseOnly<'a>,
        }
        let patch = Patch::Merge(StatusPatch {
            status: PhaseOnly {
                phase: &phase,
                message: message.as_ref(),
            },
        });
        api.patch_subresource("status", &self.name_any(), &PatchParams::default(), &patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    async fn get_root_credentials(
        &self,
        ctx: Arc<DbContext>,
        cluster_ns: &str,
        cluster_name: &str,
    ) -> Result<(String, String)> {
        // GET Cluster to read spec.rootSecret
        use kube::api::Api;
        let clusters: Api<Cluster> = Api::namespaced(ctx.client.clone(), cluster_ns);
        let cluster = clusters.get(cluster_name).await.map_err(Error::KubeError)?;
        let sec_name = cluster
            .spec
            .root_secret
            .as_ref()
            .and_then(|r| r.name.clone())
            .ok_or_else(|| Error::MissingRootCredentials)?;
        // Read secret
        let secrets: Api<k8s_openapi::api::core::v1::Secret> =
            Api::namespaced(ctx.client.clone(), cluster_ns);
        let sec = secrets.get(&sec_name).await.map_err(Error::KubeError)?;
        let data = sec.data.unwrap_or_default();
        let user = data
            .get("user")
            .map(|b| String::from_utf8(b.0.clone()).unwrap_or_default())
            .ok_or_else(|| Error::MissingRootCredentials)?;
        let pass = data
            .get("password")
            .map(|b| String::from_utf8(b.0.clone()).unwrap_or_default())
            .ok_or_else(|| Error::MissingRootCredentials)?;
        Ok((user, pass))
    }

    fn basic_auth_header(user: &str, pass: &str) -> http::HeaderValue {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        http::HeaderValue::from_str(&format!("Basic {token}")).unwrap()
    }

    async fn ensure_surreal_namespace(
        &self,
        ctx: Arc<DbContext>,
        cluster_ns: &str,
        cluster_name: &str,
        ns_name: &str,
        user: &str,
        pass: &str,
    ) -> Result<()> {
        let ns_q = format!("`{}`", ns_name.replace('`', "``"));
        let sql = format!("DEFINE NAMESPACE {ns_q};");
        let path =
            format!("/api/v1/namespaces/{cluster_ns}/services/{cluster_name}-surrealdb:8000/proxy/sql");
        let req = Request::builder()
            .method(Method::POST)
            .header(http::header::ACCEPT, HeaderValue::from_static("application/json"))
            .header(http::header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
            .header(http::header::AUTHORIZATION, Self::basic_auth_header(user, pass))
            .uri(path)
            .body(sql.into_bytes())
            .map_err(Error::HTTPError)?;
        ctx.client.request_text(req).await.map_err(Error::KubeError)?;
        Ok(())
    }

    async fn ensure_surreal_database(
        &self,
        ctx: Arc<DbContext>,
        cluster_ns: &str,
        cluster_name: &str,
        ns_name: &str,
        db_name: &str,
        user: &str,
        pass: &str,
    ) -> Result<()> {
        let ns_q = format!("`{}`", ns_name.replace('`', "``"));
        let db_q = format!("`{}`", db_name.replace('`', "``"));
        let sql = format!("USE NS {ns_q}; DEFINE DATABASE {db_q};");
        let path =
            format!("/api/v1/namespaces/{cluster_ns}/services/{cluster_name}-surrealdb:8000/proxy/sql");
        let req = Request::builder()
            .method(Method::POST)
            .header(http::header::ACCEPT, HeaderValue::from_static("application/json"))
            .header(http::header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
            .header(http::header::AUTHORIZATION, Self::basic_auth_header(user, pass))
            .uri(path)
            .body(sql.into_bytes())
            .map_err(Error::HTTPError)?;
        ctx.client.request_text(req).await.map_err(Error::KubeError)?;
        Ok(())
    }

    async fn reconcile(&self, ctx: Arc<DbContext>) -> Result<Action> {
        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or(DbPhase::New);

        match phase {
            DbPhase::New | DbPhase::InProgress => {
                let k8s_ns = self.namespace().unwrap();
                let cluster_name = &self.spec.cluster_ref.name;
                let cluster_ns = self.spec.cluster_ref.namespace.as_deref().unwrap_or(&k8s_ns);
                let surreal_ns = self.spec.db_namespace.as_deref().unwrap_or(&k8s_ns);
                let (user, pass) = self
                    .get_root_credentials(ctx.clone(), cluster_ns, cluster_name)
                    .await?;

                // Define SurrealDB namespace
                if let Err(e) = self
                    .ensure_surreal_namespace(ctx.clone(), cluster_ns, cluster_name, surreal_ns, &user, &pass)
                    .await
                {
                    self.set_phase_msg(
                        ctx.clone(),
                        DbPhase::Error,
                        Some(format!("namespace define failed: {e}")),
                    )
                    .await
                    .ok();
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                // Define SurrealDB database
                if let Err(e) = self
                    .ensure_surreal_database(
                        ctx.clone(),
                        cluster_ns,
                        cluster_name,
                        surreal_ns,
                        &self
                            .spec
                            .db_name
                            .as_ref()
                            .unwrap_or(self.meta().name.as_ref().expect("missing resource name")),
                        &user,
                        &pass,
                    )
                    .await
                {
                    self.set_phase_msg(
                        ctx.clone(),
                        DbPhase::Error,
                        Some(format!("database define failed: {e}")),
                    )
                    .await
                    .ok();
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                self.set_phase_msg(ctx.clone(), DbPhase::Ready, None).await.ok();
                Ok(Action::requeue(Duration::from_secs(60)))
            }
            DbPhase::Ready => Ok(Action::requeue(Duration::from_secs(120))),
            DbPhase::Error => Ok(Action::requeue(Duration::from_secs(30))),
        }
    }

    async fn cleanup(&self, _ctx: Arc<DbContext>) -> Result<Action> {
        // Avoid destructive cleanup (no DROP DATABASE)
        Ok(Action::await_change())
    }
}

pub async fn run_database(state: State, client: Client) {
    let dbs = Api::<Database>::all(client.clone());
    if let Err(e) = dbs.list(&ListParams::default().limit(1)).await {
        error!("Database CRD is not queryable; {e:?}");
        std::process::exit(1);
    }
    let rec = state.diagnostics.read().await.recorder(client.clone());
    Controller::new(dbs.clone(), Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile,
            error_policy,
            Arc::new(DbContext {
                client: client.clone(),
                recorder: rec,
            }),
        )
        .for_each(|_| futures::future::ready(()))
        .await;
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::database::fixtures::{DbContextMock, Scenario, timeout_after_1s};

    #[tokio::test]
    async fn new_creates_ns_and_db_then_ready() {
        let (ctx, server) = DbContextMock::test();
        let db = Database::test().finalized().with_status(DbStatus {
            phase: Some(DbPhase::New),
            message: None,
        });
        let handle = server.run(Scenario::NewEnsuresNamespaceAndDatabase(db.clone().finalized()));
        super::reconcile(Arc::new(db), ctx).await.expect("reconciled");
        timeout_after_1s(handle).await;
    }

    #[tokio::test]
    async fn finalizer_is_added_on_non_finalized() {
        let (ctx, server) = DbContextMock::test();
        let db = Database::test().with_status(DbStatus {
            phase: Some(DbPhase::New),
            message: None,
        });
        let handle = server.run(Scenario::FinalizerCreation(db.clone()));
        let _ = super::reconcile(Arc::new(db), ctx).await;
        timeout_after_1s(handle).await;
    }

    #[tokio::test]
    async fn cross_namespace_cluster_ref_uses_that_namespace() {
        let (ctx, server) = DbContextMock::test();
        let db = Database::test()
            .finalized()
            .with_cluster_ref_ns("other-ns")
            .with_status(DbStatus {
                phase: Some(DbPhase::New),
                message: None,
            });
        let handle = server.run(Scenario::NewCrossNsEnsures(db.clone().finalized()));
        super::reconcile(Arc::new(db), ctx).await.expect("reconciled");
        timeout_after_1s(handle).await;
    }

    #[tokio::test]
    async fn namespace_define_error_sets_error_phase_and_requeues() {
        let (ctx, server) = crate::database::fixtures::DbContextMock::test();
        let db = Database::test().finalized().with_status(DbStatus {
            phase: Some(DbPhase::New),
            message: None,
        });

        let handle = server.run(crate::database::fixtures::Scenario::NamespaceDefineError(
            db.clone().finalized(),
        ));
        super::reconcile(Arc::new(db), ctx).await.expect("reconciled");
        crate::database::fixtures::timeout_after_1s(handle).await;
    }

    #[tokio::test]
    async fn database_define_error_sets_error_phase_and_requeues() {
        let (ctx, server) = crate::database::fixtures::DbContextMock::test();
        let db = Database::test().finalized().with_status(DbStatus {
            phase: Some(DbPhase::New),
            message: None,
        });
        let handle = server.run(crate::database::fixtures::Scenario::DatabaseDefineError(
            db.clone().finalized(),
        ));
        super::reconcile(Arc::new(db), ctx).await.expect("reconciled");
        crate::database::fixtures::timeout_after_1s(handle).await;
    }
}
