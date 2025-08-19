use crate::{Cluster, Database, Error, Result, run::State};

use futures::StreamExt;
use kube::{
    CustomResource, Resource,
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        finalizer::{Event as Finalizer, finalizer},
        watcher::Config,
    },
};

use base64::Engine;
use rand::{Rng, distr::Alphanumeric, rng};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tracing::*;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "User",
    group = "surrealdb.aexol.com",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "UserStatus", shortname = "susr")]
#[serde(rename_all = "camelCase")]
pub struct UserSpec {
    pub cluster_ref: ClusterRef,
    pub database_ref: DatabaseRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserStatus {
    #[serde(default)]
    pub phase: Option<UserPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum UserPhase {
    #[default]
    New,
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

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::user::fixtures::{Scenario, UserContextMock, timeout_after_1s};

    #[tokio::test]
    async fn finalizer_is_added_on_non_finalized() {
        let (ctx, server) = UserContextMock::test();
        let user = super::User::test().with_status(UserStatus {
            phase: Some(UserPhase::New),
            message: None,
        });
        let handle = server.run(Scenario::FinalizerCreation(user.clone()));
        let _ = super::reconcile(Arc::new(user), ctx).await;
        timeout_after_1s(handle).await;
    }

    #[tokio::test]
    async fn new_creates_db_user_and_secret_then_ready() {
        let (ctx, server) = UserContextMock::test();
        let user = super::User::test().finalized().with_status(UserStatus {
            phase: Some(UserPhase::New),
            message: None,
        });
        let handle = server.run(Scenario::NewCreatesUserAndSecretAndConfigMapDefaultNames(
            user.clone(),
        ));
        super::reconcile(Arc::new(user), ctx).await.expect("reconciled");
        timeout_after_1s(handle).await;
    }
}

#[derive(Clone)]
pub struct UserContext {
    pub client: Client,
}

impl UserSpec {
    fn effective_username<'a>(&'a self, name: &'a str) -> &'a str {
        self.username.as_deref().unwrap_or(name)
    }
}

impl User {
    async fn set_phase_msg(
        &self,
        ctx: Arc<UserContext>,
        phase: UserPhase,
        message: Option<String>,
    ) -> Result<()> {
        let ns = self.namespace().unwrap();
        let api: Api<super::crd::User> = Api::namespaced(ctx.client.clone(), &ns);
        #[derive(serde::Serialize, Debug)]
        struct PhaseOnly<'a> {
            phase: &'a UserPhase,
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

    async fn ensure_secret(&self, ctx: Arc<UserContext>, password: &str) -> Result<String> {
        use k8s_openapi::api::core::v1::Secret;
        use kube::api::PostParams;
        let ns = self.namespace().unwrap();
        let name = self
            .spec
            .secret_name
            .clone()
            .unwrap_or_else(|| format!("{}-credentials", self.name_any()));
        let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let mut data = std::collections::BTreeMap::new();
        data.insert(
            "password".to_string(),
            k8s_openapi::ByteString(password.as_bytes().to_vec()),
        );
        let sec = Secret {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.clone()),
                owner_references: self.controller_owner_ref(&()).map(|mut o| {
                    o.controller = Some(true);
                    vec![o]
                }),
                ..Default::default()
            },
            type_: Some("Opaque".into()),
            data: Some(data),
            ..Default::default()
        };
        match secrets.create(&PostParams::default(), &sec).await {
            Ok(_) => {}
            Err(kube::Error::Api(ae)) if ae.code == 409 => {}
            Err(e) => return Err(Error::KubeError(e)),
        }
        Ok(name)
    }

    async fn ensure_configmap(
        &self,
        ctx: Arc<UserContext>,
        username: &str,
        cluster_ns: &str,
        cluster_name: &str,
    ) -> Result<String> {
        use k8s_openapi::api::core::v1::ConfigMap;
        use kube::api::PostParams;
        let ns = self.namespace().unwrap();
        let name = format!("{}-config", self.name_any());
        let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
        let url = format!("http://{cluster_name}-surrealdb.{cluster_ns}.svc:8000");
        let data: std::collections::BTreeMap<String, String> = [
            ("username".to_string(), username.to_string()),
            ("url".to_string(), url),
        ]
        .into_iter()
        .collect();
        let cm = ConfigMap {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.clone()),
                owner_references: self.controller_owner_ref(&()).map(|mut o| {
                    let _ = o.controller.replace(true);
                    vec![o]
                }),
                ..Default::default()
            },
            data: Some(data),
            ..Default::default()
        };
        match cms.create(&PostParams::default(), &cm).await {
            Ok(_) => {}
            Err(kube::Error::Api(ae)) if ae.code == 409 => {}
            Err(e) => return Err(Error::KubeError(e)),
        }
        Ok(name)
    }

    async fn get_root_credentials_for_cluster(
        &self,
        ctx: Arc<UserContext>,
        cluster_ns: &str,
        cluster_name: &str,
    ) -> Result<(String, String)> {
        // Reuse Database helper via trait bounds not available; copy logic
        let clusters: Api<Cluster> = Api::namespaced(ctx.client.clone(), cluster_ns);
        let cluster = clusters.get(cluster_name).await.map_err(Error::KubeError)?;
        let sec_name = cluster
            .spec
            .root_secret
            .as_ref()
            .and_then(|r| r.name.clone())
            .ok_or(Error::MissingRootCredentials)?;
        let secrets: Api<k8s_openapi::api::core::v1::Secret> =
            Api::namespaced(ctx.client.clone(), cluster_ns);
        let sec = secrets.get(&sec_name).await.map_err(Error::KubeError)?;
        let data = sec.data.unwrap_or_default();
        let user = data
            .get("user")
            .map(|b| String::from_utf8(b.0.clone()).unwrap_or_default())
            .ok_or(Error::MissingRootCredentials)?;
        let pass = data
            .get("password")
            .map(|b| String::from_utf8(b.0.clone()).unwrap_or_default())
            .ok_or(Error::MissingRootCredentials)?;
        Ok((user, pass))
    }

    fn basic_auth_header(user: &str, pass: &str) -> http::HeaderValue {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        http::HeaderValue::from_str(&format!("Basic {token}")).unwrap()
    }

    async fn provision_user(
        &self,
        ctx: Arc<UserContext>,
        cluster_ns: &str,
        cluster_name: &str,
        ns_name: &str,
        db_name: &str,
        username: &str,
        password: &str,
        root_user: &str,
        root_pass: &str,
    ) -> Result<()> {
        use http::header::{ACCEPT, CONTENT_TYPE};
        use http::{Method, Request};
        let ns_q = format!("`{}`", ns_name.replace('`', "``"));
        let db_q = format!("`{}`", db_name.replace('`', "``"));
        let user_q = format!("`{}`", username.replace('`', "``"));
        let pw_esc = password.replace('\'', "''");
        let sql = format!(
            "USE NS {ns_q}; USE DB {db_q}; DEFINE USER {user_q} ON DATABASE PASSWORD '{pw_esc}' ROLES EDITOR;"
        );
        let path =
            format!("/api/v1/namespaces/{cluster_ns}/services/{cluster_name}-surrealdb:8000/proxy/sql");
        let req = Request::builder()
            .method(Method::POST)
            .header(ACCEPT, http::HeaderValue::from_static("application/json"))
            .header(CONTENT_TYPE, http::HeaderValue::from_static("text/plain"))
            .header(
                http::header::AUTHORIZATION,
                Self::basic_auth_header(root_user, root_pass),
            )
            .uri(path)
            .body(sql.into_bytes())
            .map_err(Error::HTTPError)?;
        ctx.client.request_text(req).await.map_err(Error::KubeError)?;
        Ok(())
    }

    async fn reconcile(&self, ctx: Arc<UserContext>) -> Result<Action> {
        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or(UserPhase::New);
        match phase {
            UserPhase::New | UserPhase::Error => {
                let k8s_ns = self.namespace().unwrap();
                let cluster_name = &self.spec.cluster_ref.name;
                let cluster_ns = self.spec.cluster_ref.namespace.as_deref().unwrap_or(&k8s_ns);
                let db_name = &self.spec.database_ref.name;
                let db_ns = self.spec.database_ref.namespace.as_deref().unwrap_or(&k8s_ns);
                // Resolve db resource to read its namespace/name defaults
                let dbs: Api<Database> = Api::namespaced(ctx.client.clone(), db_ns);
                let db = dbs.get(db_name).await.map_err(Error::KubeError)?;
                let surreal_ns = db.spec.db_namespace.clone().unwrap_or(db_ns.to_string());
                let surreal_db = db.spec.db_name.clone().unwrap_or(db.name_any());
                let (root_user, root_pass) = self
                    .get_root_credentials_for_cluster(ctx.clone(), cluster_ns, cluster_name)
                    .await?;
                let username = self.spec.effective_username(self.meta().name.as_ref().unwrap());
                let password = self.spec.password.clone().unwrap_or_else(|| {
                    let mut s: String = rng()
                        .sample_iter(&Alphanumeric)
                        .take(20)
                        .map(char::from)
                        .collect();
                    const SYMBOLS: &[u8] = br"!@#$%^&*()-_=+[]{};:,.?~";
                    for _ in 0..4 {
                        let idx = rng().random_range(0..(s.len().max(1)));
                        let sym = SYMBOLS[rng().random_range(0..SYMBOLS.len())] as char;
                        s.insert(idx, sym);
                    }
                    s
                });
                if let Err(e) = self
                    .provision_user(
                        ctx.clone(),
                        cluster_ns,
                        cluster_name,
                        &surreal_ns,
                        &surreal_db,
                        username,
                        &password,
                        &root_user,
                        &root_pass,
                    )
                    .await
                {
                    self.set_phase_msg(
                        ctx.clone(),
                        UserPhase::Error,
                        Some(format!("provision failed: {e}")),
                    )
                    .await
                    .ok();
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                if let Err(e) = self.ensure_secret(ctx.clone(), &password).await {
                    self.set_phase_msg(ctx.clone(), UserPhase::Error, Some(format!("secret failed: {e}")))
                        .await
                        .ok();
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                if let Err(e) = self
                    .ensure_configmap(ctx.clone(), username, cluster_ns, cluster_name)
                    .await
                {
                    self.set_phase_msg(
                        ctx.clone(),
                        UserPhase::Error,
                        Some(format!("configmap failed: {e}")),
                    )
                    .await
                    .ok();
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                self.set_phase_msg(ctx.clone(), UserPhase::Ready, None).await.ok();
                Ok(Action::requeue(Duration::from_secs(120)))
            }
            UserPhase::Ready => Ok(Action::requeue(Duration::from_secs(180))),
        }
    }

    async fn cleanup(&self, _ctx: Arc<UserContext>) -> Result<Action> {
        Ok(Action::await_change())
    }
}

#[instrument(skip(ctx, user), fields(trace_id))]
async fn reconcile(user: Arc<super::crd::User>, ctx: Arc<UserContext>) -> Result<Action> {
    let ns = user.namespace().unwrap();
    let api: Api<super::crd::User> = Api::namespaced(ctx.client.clone(), &ns);
    info!("Reconciling USer \"{}\" in {}", user.name_any(), ns);
    finalizer(&api, "users.surrealdb.aexol.com", user, |event| async {
        match event {
            Finalizer::Apply(u) => u.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(u) => u.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn error_policy(_user: Arc<super::crd::User>, _err: &Error, _ctx: Arc<UserContext>) -> Action {
    Action::requeue(Duration::from_secs(60))
}

pub async fn run_user(_state: State, client: Client) {
    let users = Api::<super::crd::User>::all(client.clone());
    if let Err(e) = users.list(&ListParams::default().limit(1)).await {
        error!("User CRD is not queryable; {e:?}");
        std::process::exit(1);
    }
    Controller::new(users.clone(), Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile, error_policy, Arc::new(UserContext { client }))
        .for_each(|_| futures::future::ready(()))
        .await;
}
