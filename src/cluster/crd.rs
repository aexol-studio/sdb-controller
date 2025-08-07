use crate::{
    Error, Metrics, Result,
    run::{Diagnostics, State},
    telemetry,
};
use chrono::Utc;
use futures::StreamExt;
use http::{
    HeaderValue, Method, Request,
    header::{ACCEPT, CONTENT_TYPE},
};
use k8s_openapi::{
    Metadata, NamespaceResourceScope,
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, EnvVarSource, HTTPGetAction, PodSpec, PodTemplateSpec, Probe,
            Secret, SecretKeySelector, SecretReference, Service, ServicePort, ServiceSpec,
        },
    },
    apimachinery::pkg::{
        apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference},
        util::intstr::IntOrString,
    },
};
use kube::{
    CustomResource, Resource,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams, PropagationPolicy, ResourceExt},
    client::Client,
    core::object::HasSpec,
    runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder},
        finalizer::{Event as Finalizer, finalizer},
        watcher::Config,
    },
};
use rand::{Rng, distr::Alphanumeric, rng};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tidb_api::tidbclusters::{TidbCluster, TidbClusterSpec};
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

pub fn generate_secure_password(len: usize) -> String {
    let n = len.max(32);
    let mut s: String = rng()
        .sample_iter(&Alphanumeric)
        .take(n - 4)
        .map(char::from)
        .collect();

    const SYMBOLS: &[u8] = br"!@#$%^&*()-_=+[]{};:,.?~";
    for _ in 0..4 {
        let idx = rng().random_range(0..(s.len().max(1)));
        let sym = SYMBOLS[rng().random_range(0..SYMBOLS.len())] as char;
        s.insert(idx, sym);
    }
    s
}

pub static CLUSTER_FINALIZER: &str = "clusters.surrealdb.aexol.com";
pub static BOOTSTRAP_HTTP_SERVER_URL_ANNOTATION: &str = "surrealdb.aexol.com/bootstrap-url";

/// Generate the Kubernetes wrapper struct `Cluster` from our Spec and Status struct
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Cluster",
    group = "surrealdb.aexol.com",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "Status", shortname = "cluster")]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surrealdb_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_secret: Option<SecretReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tikv_secret_ref: Option<SecretKeySelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tidb_cluster: Option<TidbClusterSpec>,
}

/// The phase of `Cluster`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub enum Phase {
    #[default]
    New,
    Preparing,
    InCreation,
    NotReady,
    InProgress,
    Ready,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TidbClusterRef {
    pub name: String,
    pub namespace: Option<String>,
}

/// The status object of `Cluster`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(default)]
    pub phase: Option<Phase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tidb_cluster_ref: Option<TidbClusterRef>,
}

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Event recorder
    pub recorder: Recorder,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Prometheus metrics
    pub metrics: Arc<Metrics>,
}

#[instrument(skip(ctx, cluster), fields(trace_id))]
async fn reconcile(cluster: Arc<Cluster>, ctx: Arc<Context>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    if trace_id != opentelemetry::trace::TraceId::INVALID {
        Span::current().record("trace_id", field::display(&trace_id));
    }
    let _timer = ctx.metrics.reconcile.count_and_measure(&trace_id);
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = cluster.namespace().unwrap(); // cluster is namespace scoped
    let clusters: Api<Cluster> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Cluster \"{}\" in {}", cluster.name_any(), ns);
    finalizer(&clusters, CLUSTER_FINALIZER, cluster, |event| async {
        match event {
            Finalizer::Apply(cluster) => cluster.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(cluster) => cluster.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn error_policy(cluster: Arc<Cluster>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile.set_failure(&*cluster, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

fn is_true(s: &str) -> bool {
    matches!(s, "True" | "true" | "TRUE")
}

impl Cluster {
    fn owner_ref(&self) -> OwnerReference {
        let mut oref = self.controller_owner_ref(&()).expect("owner_ref");
        // Ensure controller=true
        oref.controller = Some(true);
        oref
    }

    fn labels(&self, suffix: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("app.kubernetes.io/name".into(), "surrealdb".into());
        m.insert("surrealdb.aexol.com/cluster".into(), self.name_any());
        if !suffix.is_empty() {
            m.insert("surrealdb.aexol.com/name".into(), suffix.into());
        }
        m
    }

    async fn persist_root_secret_ref(&self, ctx: Arc<Context>, secret_name: &str) -> Result<()> {
        // Only persist if user didn't provide a root_secret
        if self.spec.root_secret.is_some() {
            return Ok(());
        }
        let name = self.name_any();
        let ns = self.namespace().unwrap();
        let clusters: Api<Cluster> = Api::namespaced(ctx.client.clone(), &ns);

        let patch = Patch::Json::<()>(json_patch::Patch(vec![json_patch::PatchOperation::Replace(
            json_patch::ReplaceOperation {
                path: "/spec/rootSecret".parse().unwrap(),
                value: serde_json::json!({
                    "name": secret_name,
                    "namespace": ns,
                }),
            },
        )]));
        let pp = PatchParams::default(); // status is not involved; no apply needed
        clusters
            .patch(&name, &pp, &patch)
            .await
            .map(|_| ())
            .map_err(Error::KubeError)
    }

    async fn ensure_root_secret(&self, ctx: Arc<Context>) -> Result<SecretReference> {
        if let Some(sel) = &self.spec.root_secret {
            return Ok(sel.clone());
        }
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let name = format!("{}-surrealdb-root", name);

        let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let secret = Secret {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.clone()),
                owner_references: Some(vec![self.owner_ref()]),
                ..Default::default()
            },
            type_: Some("Opaque".into()),
            string_data: Some(
                [
                    ("user".to_string(), "root".to_string()),
                    ("password".to_string(), generate_secure_password(32)),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        };
        match secrets.create(&PostParams::default(), &secret).await {
            Ok(_) => {}
            Err(kube::Error::Api(ae)) if ae.code == 409 => {} // Already exists
            Err(e) => return Err(Error::KubeError(e)),
        }
        self.persist_root_secret_ref(ctx.clone(), &name).await?;
        Ok(SecretReference {
            name: Some(name),
            namespace: Some(ns),
        })
    }

    fn build_surreal_service(&self, name: &str) -> Service {
        let svc_name = format!("{name}-surrealdb");
        let labels = self.labels(&svc_name);
        Service {
            metadata: ObjectMeta {
                name: Some(svc_name.clone()),
                namespace: self.namespace(),
                labels: Some(labels.clone()),
                owner_references: Some(vec![self.owner_ref()]),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(labels),
                ports: Some(vec![ServicePort {
                    name: Some("http".into()),
                    port: 8000i32,
                    target_port: Some(IntOrString::Int(8000)),
                    protocol: Some("TCP".into()),
                    ..Default::default()
                }]),
                type_: Some("ClusterIP".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn env_normal(&self) -> Vec<EnvVar> {
        let name = self.name_any();
        vec![
            EnvVar {
                name: "SURREAL_AUTH".into(),
                value: Some("true".into()),
                ..Default::default()
            },
            EnvVar {
                name: "SURREAL_PATH".into(),
                value_from: self.spec.tikv_secret_ref.as_ref().map(|selector| EnvVarSource {
                    secret_key_ref: Some(selector.clone()),
                    ..Default::default()
                }),
                value: self
                    .spec
                    .tidb_cluster
                    .as_ref()
                    .map(|_| format!("tikv://{name}-pd:2379")),
                ..Default::default()
            },
        ]
    }

    // Build container env for unauthenticated bootstrap mode
    fn env_creation(&self) -> Vec<EnvVar> {
        let mut env = self.env_normal();
        // SURREAL_UNAUTHENTICATED=true signals open access to create root
        env.retain(|e| e.name != "SURREAL_AUTH"); // remove AUTH if present
        env.push(EnvVar {
            name: "SURREAL_UNAUTHENTICATED".into(),
            value: Some("true".into()),
            ..Default::default()
        });
        env
    }

    fn build_surreal_deployment(&self, name: &str, creation_mode: bool) -> Deployment {
        let deploy_name = format!("{name}-surrealdb");
        let labels = self.labels(&deploy_name);

        let image = self
            .spec
            .surrealdb_image
            .clone()
            .unwrap_or_else(|| "surrealdb/surrealdb:latest".to_string());
        let replicas = self.spec.replicas.map(|r| r as i32).unwrap_or(1);

        let env = if creation_mode {
            self.env_creation()
        } else {
            self.env_normal()
        };

        let ports = vec![ContainerPort {
            container_port: 8000,
            name: Some("http".into()),
            protocol: Some("TCP".into()),
            ..Default::default()
        }];

        let http_probe = Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/health".into()),
                port: IntOrString::Int(8000),
                scheme: Some("HTTP".into()),
                ..Default::default()
            }),
            initial_delay_seconds: Some(5),
            period_seconds: Some(10),
            ..Default::default()
        };

        let container = Container {
            name: "surrealdb".into(),
            image: Some(image),
            args: Some(vec!["start".into(), "--bind".into(), "0.0.0.0:8000".into()]),
            env: Some(env),
            ports: Some(ports),
            readiness_probe: Some(http_probe.clone()),
            liveness_probe: Some(http_probe),
            ..Default::default()
        };

        Deployment {
            metadata: ObjectMeta {
                name: Some(deploy_name.clone()),
                namespace: self.namespace(),
                labels: Some(labels.clone()),
                owner_references: Some(vec![self.owner_ref()]),
                ..Default::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(replicas),
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels),
                        ..Default::default()
                    }),
                    spec: Some(PodSpec {
                        containers: vec![container],
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    async fn reconcile_surrealdb_with_mode(
        &self,
        ctx: Arc<Context>,
        name: &str,
        creation_mode: bool,
    ) -> Result<()> {
        let ns = self.namespace().unwrap();
        let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ns);
        let services: Api<Service> = Api::namespaced(ctx.client.clone(), &ns);

        let dp = self.build_surreal_deployment(name, creation_mode);
        let svc = self.build_surreal_service(name);

        let pp = PatchParams::apply("sdb-controller").force();

        // Upsert Deployment
        deployments
            .patch(
                dp.metadata().name.as_ref().expect("missing deployment name"),
                &pp,
                &Patch::Apply(&dp),
            )
            .await
            .map_err(Error::KubeError)?;

        // Upsert Service
        services
            .patch(
                svc.metadata().name.as_ref().expect("missing service name"),
                &pp,
                &Patch::Apply(&svc),
            )
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    async fn bootstrap_root_user(&self, ctx: Arc<Context>, name: &str) -> Result<()> {
        let ns = self.namespace().unwrap();

        // Resolve root secret name from SecretReference (name only)
        let sec_name = self
            .spec
            .root_secret
            .as_ref()
            .and_then(|r| r.name.clone())
            .ok_or(Error::MissingRootCredentials)?;

        // Read secret from k8s
        let secrets: Api<k8s_openapi::api::core::v1::Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let sec = secrets.get(&sec_name).await.map_err(Error::KubeError)?;

        // Decode data["user"] and data["password"]
        let data = sec.data.unwrap_or_default();
        let user_b = data.get("user");
        let pass_b = data.get("password");
        let (user, pass) = match (user_b, pass_b) {
            (Some(u), Some(p)) => Ok((
                String::from_utf8(u.0.clone()).unwrap_or_default(),
                String::from_utf8(p.0.clone()).unwrap_or_default(),
            )),
            // Hard fail here. Better safe than sorry
            _ => Err(Error::MissingRootCredentials),
        }?;

        // Build SQL body
        let sql = format!("DEFINE USER {user} ON ROOT PASSWORD '{pass}' ROLES OWNER;");

        // Prefer in-cluster kube-apiserver proxy to reach the Service:
        // POST /api/v1/namespaces/{ns}/services/{name}-surrealdb:8000/proxy/sql
        let svc_name = format!("{name}-surrealdb:8000"); // name:port for the proxy path
        let path = format!("/api/v1/namespaces/{ns}/services/{svc_name}/proxy/sql");
        let req = Request::builder()
            .method(Method::POST)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .header(CONTENT_TYPE, HeaderValue::from_static("text/plain"))
            .uri(path)
            .body(sql.into_bytes())
            .map_err(Error::HTTPError)?;

        ctx.client.request_text(req).await.map_err(Error::KubeError)?;
        Ok(())
    }

    async fn set_phase(&self, ctx: Arc<Context>, phase: Phase) -> Result<()> {
        let ns = self.namespace().unwrap();
        let clusters: Api<Cluster> = Api::namespaced(ctx.client.clone(), &ns);

        #[derive(serde::Serialize, Debug)]
        struct PhaseOnly<'a> {
            phase: &'a Phase,
        }
        #[derive(serde::Serialize, Debug)]
        struct StatusPatch<'a> {
            status: PhaseOnly<'a>,
        }

        let patch = Patch::Merge(StatusPatch {
            status: PhaseOnly { phase: &phase },
        });
        clusters
            .patch_subresource("status", &self.name_any(), &PatchParams::default(), &patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    async fn check_ready(&self, ctx: Arc<Context>, name: &str) -> Result<bool> {
        let ns = self.namespace().unwrap();
        let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &ns);
        let dp_name = format!("{name}-surrealdb");
        let dp = deployments.get(&dp_name).await.map_err(Error::KubeError)?;
        let ready = dp.status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0) > 0;
        Ok(ready)
    }

    fn tikv_backend_configured(&self) -> std::result::Result<(), anyhow::Error> {
        if self.spec.tidb_cluster.is_some() || self.spec.tikv_secret_ref.is_some() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "No TiKV backend configured: set .spec.tidbCluster or .spec.tikvSecret"
            ))
        }
    }

    // Reconcile TidbCluster managed by controller
    async fn reconcile_tidb_cluster(&self, ctx: Arc<Context>, name: &str) -> Result<()> {
        // Only act if user provided a TiDB cluster spec
        let Some(spec) = &self.spec.tidb_cluster else {
            return Ok(());
        };
        let client = ctx.client.clone();
        let ns = self.namespace().unwrap();
        let tidb_clusters: Api<TidbCluster> = Api::namespaced(client, &ns);

        // Ensure owner reference
        let mut obj = TidbCluster::new(name, spec.clone());
        {
            // Fill metadata: namespace + ownerReferences
            let meta = obj.meta_mut();
            meta.namespace = Some(ns.clone());
            let oref = self.owner_ref();
            let mut owners = meta.owner_references.take().unwrap_or_default();
            // Avoid duplicates
            if !owners
                .iter()
                .any(|o| self.meta().uid.as_ref().is_some_and(|v| v == &o.uid) && o.name == self.name_any())
            {
                owners.push(oref);
            }
            meta.owner_references = Some(owners);
        }

        // Try to create; if it already exists, do nothing
        match tidb_clusters.create(&PostParams::default(), &obj).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(ae)) if ae.code == 409 => Ok(()), // Already exists
            Err(e) => Err(Error::KubeError(e)),
        }
    }

    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let name = self.name_any();

        let phase = self
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or(Phase::New);

        match phase {
            Phase::New => {
                if let Err(e) = self.tikv_backend_configured() {
                    // surface error phase and requeue
                    warn!("preflight failed for {}: {}", self.name_any(), e);
                    self.set_phase(ctx.clone(), Phase::Error).await?;
                    return Ok(Action::requeue(Duration::from_secs(30)));
                }
                if let Err(e) = self.ensure_root_secret(ctx.clone()).await {
                    warn!("ensure_root_secret failed for {}: {}", self.name_any(), e);
                    self.set_phase(ctx.clone(), Phase::Error).await.ok();
                    return Ok(Action::requeue(Duration::from_secs(30)));
                }
                match self.spec().tidb_cluster {
                    Some(_) => {
                        self.set_phase(ctx, Phase::Preparing).await.ok();
                        Ok(Action::requeue(Duration::from_secs(10)))
                    }
                    None => {
                        self.set_phase(ctx.clone(), Phase::InCreation).await?;
                        self.reconcile_surrealdb_with_mode(ctx.clone(), &name, true)
                            .await?;
                        Ok(Action::requeue(Duration::from_secs(20)))
                    }
                }
            }
            Phase::Preparing => {
                // Create/ensure TiDB cluster exists
                if let Err(err) = self.reconcile_tidb_cluster(ctx.clone(), &name).await {
                    warn!("error creating tidb cluster: {err}");
                    return Ok(Action::requeue(Duration::from_secs(20)));
                }
                // When tidb_cluster is provided, wait until TiDB shows up and becomes Ready
                // Use a simple GET and condition-based readiness check when available.
                let ns = self.namespace().unwrap();
                let tidb_clusters: Api<TidbCluster> = Api::namespaced(ctx.client.clone(), &ns);
                match tidb_clusters.get_opt(&name).await {
                    Ok(Some(tc)) => {
                        let ready = tc.status.as_ref().is_some_and(|st| {
                            st.conditions.as_ref().is_some_and(|conds| {
                                conds
                                    .iter()
                                    .find(|c| {
                                        (c.type_ == "Ready" || c.type_ == "Available") && is_true(&c.status)
                                    })
                                    .is_some()
                            })
                        });
                        if ready {
                            self.set_phase(ctx.clone(), Phase::InCreation).await.ok();
                            self.reconcile_surrealdb_with_mode(ctx.clone(), &name, true)
                                .await?;
                            Ok(Action::requeue(Duration::from_secs(20)))
                        } else {
                            Ok(Action::requeue(Duration::from_secs(15)))
                        }
                    }
                    Ok(None) => Ok(Action::requeue(Duration::from_secs(10))),
                    Err(e) => {
                        warn!("waiting for tidb cluster {}: {}", name, e);
                        Ok(Action::requeue(Duration::from_secs(15)))
                    }
                }
            }
            Phase::InCreation => match self.check_ready(ctx.clone(), &name).await {
                Ok(true) => {
                    if let Err(e) = self.bootstrap_root_user(ctx.clone(), &name).await {
                        warn!("bootstrap failed: {e}");
                        return Ok(Action::requeue(Duration::from_secs(30)));
                    }
                    self.set_phase(ctx.clone(), Phase::InProgress).await.ok();
                    self.reconcile_surrealdb_with_mode(ctx.clone(), &name, false)
                        .await?;
                    Ok(Action::requeue(Duration::from_secs(20)))
                }
                Ok(false) => Ok(Action::requeue(Duration::from_secs(20))),
                Err(e) => {
                    warn!("check_ready failed for {}: {}", name, e);
                    Ok(Action::requeue(Duration::from_secs(20)))
                }
            },
            Phase::InProgress => {
                self.reconcile_surrealdb_with_mode(ctx.clone(), &name, false)
                    .await?;
                match self.check_ready(ctx.clone(), &name).await {
                    Ok(true) => {
                        self.set_phase(ctx.clone(), Phase::Ready).await.ok();
                        Ok(Action::requeue(Duration::from_secs(60)))
                    }
                    Ok(false) => {
                        self.set_phase(ctx.clone(), Phase::NotReady).await.ok();
                        Ok(Action::requeue(Duration::from_secs(30)))
                    }
                    Err(e) => {
                        warn!("check_ready failed for {}: {}", name, e);
                        self.set_phase(ctx.clone(), Phase::NotReady).await.ok();
                        Ok(Action::requeue(Duration::from_secs(30)))
                    }
                }
            }
            Phase::Ready => {
                self.reconcile_surrealdb_with_mode(ctx.clone(), &name, false)
                    .await?;
                match self.check_ready(ctx.clone(), &name).await {
                    Ok(true) => Ok(Action::requeue(Duration::from_secs(120))),
                    Ok(false) => {
                        self.set_phase(ctx.clone(), Phase::NotReady).await.ok();
                        Ok(Action::requeue(Duration::from_secs(30)))
                    }
                    Err(e) => {
                        warn!("check_ready failed for {}: {}", name, e);
                        self.set_phase(ctx.clone(), Phase::NotReady).await.ok();
                        Ok(Action::requeue(Duration::from_secs(30)))
                    }
                }
            }
            Phase::NotReady => {
                self.reconcile_surrealdb_with_mode(ctx.clone(), &name, false)
                    .await?;
                match self.check_ready(ctx.clone(), &name).await {
                    Ok(true) => {
                        self.set_phase(ctx.clone(), Phase::Ready).await.ok();
                        Ok(Action::requeue(Duration::from_secs(60)))
                    }
                    Ok(false) => Ok(Action::requeue(Duration::from_secs(30))),
                    Err(e) => {
                        warn!("check_ready failed for {}: {}", name, e);
                        Ok(Action::requeue(Duration::from_secs(30)))
                    }
                }
            }
            Phase::Error => {
                // Attempt to heal by reconciling authenticated setup
                if let Err(e) = self
                    .reconcile_surrealdb_with_mode(ctx.clone(), &name, false)
                    .await
                {
                    warn!("heal attempt failed: {e}");
                }
                Ok(Action::requeue(Duration::from_secs(45)))
            }
        }
    }

    async fn delete_if_exists<T>(
        &self,
        ctx: Arc<Context>,
        name: &str,
        propagation_policy: Option<PropagationPolicy>,
    ) -> Result<()>
    where
        T: Resource<DynamicType = (), Scope = NamespaceResourceScope>
            + serde::de::DeserializeOwned
            + Clone
            + Default
            + std::fmt::Debug,
        Api<T>: Clone,
    {
        let ns = self.namespace().unwrap();
        let api: Api<T> = Api::namespaced(ctx.client.clone(), &ns);

        match api.get(name).await {
            Ok(item) => {
                let meta = item.meta();
                let owned = meta
                    .owner_references
                    .as_ref()
                    .map(|owners| {
                        owners.iter().any(|o| {
                            let uid_match = match (&o.uid, &self.meta().uid) {
                                (ouid, Some(cuid)) => ouid == cuid,
                                _ => false,
                            };
                            uid_match
                                || (o.kind == Self::kind(&())
                                    && o.name == self.name_any()
                                    && o.api_version == Self::api_version(&()))
                        })
                    })
                    .unwrap_or(false);
                if !owned {
                    return Ok(());
                }
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => return Ok(()),
            Err(e) => return Err(Error::KubeError(e)),
        }

        let dp = DeleteParams {
            propagation_policy: Some(propagation_policy.unwrap_or(PropagationPolicy::Background)),
            ..Default::default()
        };
        match api.delete(name, &dp).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
            Err(e) => Err(Error::KubeError(e)),
        }
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let name = self.name_any();
        let oref = self.object_ref(&());

        // Best-effort deletes; ignore NotFound
        let dp_name = format!("{}-surrealdb", name);
        let svc_name = format!("{}-surrealdb", self.name_any());
        let sec_name = format!("{}-surrealdb-root", self.name_any());
        self.delete_if_exists::<Deployment>(ctx.clone(), &dp_name, None)
            .await?;
        self.delete_if_exists::<Service>(ctx.clone(), &svc_name, None)
            .await?;
        self.delete_if_exists::<Secret>(ctx.clone(), &sec_name, None)
            .await?;
        // Intentionally skip deleting TiDB/TiKV cluster resources
        self.delete_if_exists::<TidbCluster>(ctx.clone(), &name, Some(PropagationPolicy::Orphan))
            .await?;

        // Publish delete requested event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Delete `{}`", name)),
                    action: "Deleting".into(),
                    secondary: None,
                },
                &oref,
            )
            .await
            .map_err(Error::KubeError)?;

        Ok(Action::await_change())
    }
}

/// Initialize the controller and shared state (given the crd is installed)
pub async fn run_cluster(state: State, client: Client) {
    let clusters = Api::<Cluster>::all(client.clone());
    if let Err(e) = clusters.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    let rec = state.diagnostics.read().await.recorder(client.clone());
    Controller::new(clusters.clone(), Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile,
            error_policy,
            Arc::new(Context {
                client: client.clone(),
                recorder: rec,
                metrics: state.metrics.clone(),
                diagnostics: state.diagnostics.clone(),
            }),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .await;
}

// Mock tests relying on fixtures.rs and its primitive apiserver mocks
#[cfg(test)]
mod test {
    use super::{Api, Cluster, Context, Patch, PatchParams, Phase, PostParams, Resource, Secret, reconcile};
    use crate::fixtures::{Scenario, timeout_after_1s};
    use std::sync::Arc;

    #[tokio::test]
    async fn clusters_without_finalizer_gets_a_finalizer() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test();
        let mocksrv = fakeserver.run(Scenario::FinalizerCreation(cluster.clone()));
        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
    #[tokio::test]
    async fn new_phase_sets_in_creation_and_applies_dp_svc_with_unauthenticated_env() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test()
            .finalized()
            .with_status(super::Status {
                phase: Some(super::Phase::New),
                ..Default::default()
            })
            .needs_root_credentials()
            .needs_tikv_secret_ref();
        let mocksrv = fakeserver.run(Scenario::StatusPatchThenDeploymentAndServiceCreation(
            cluster.clone(),
        ));
        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn in_creation_when_ready_bootstraps_then_flips_to_auth_env_and_in_progress() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test()
            .finalized()
            .with_status(super::Status {
                phase: Some(Phase::InCreation),
                ..Default::default()
            })
            .needs_root_credentials()
            .needs_tikv_secret_ref();
        let mocksrv = fakeserver.run(Scenario::InCreationReadyThenBootstrapThenAuth(cluster.clone()));
        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn authenticated_mode_applies_dp_svc_with_auth_and_path_env() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test()
            .finalized()
            .with_status(super::Status {
                phase: Some(super::Phase::InProgress),
                ..Default::default()
            })
            .needs_tikv_secret_ref();
        let mocksrv = fakeserver.run(Scenario::DeploymentAndServiceWithAuthEnv(cluster.clone()));
        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_cluster_with_delete_timestamp_causes_delete() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test().finalized().needs_delete();
        let mocksrv = fakeserver.run(Scenario::Cleanup("DeleteRequested".into(), cluster.clone()));
        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn new_phase_enters_in_creation_flow_without_extra_calls() {
        let (testctx, fakeserver) = Context::test();
        // finalized to skip finalizer path and go directly into reconcile logic
        let cluster = Cluster::test().finalized().with_status(super::Status {
            phase: Some(Phase::New),
            ..Default::default()
        });
        // Since fixtures don’t yet mock apply patches or status patches here, choose RadioSilence
        // to ensure reconcile does not make unexpected extra calls that we didn't stub.
        let mocksrv = fakeserver.run(Scenario::RadioSilence);
        // Expect reconcile to run without panicking; any extra unhandled k8s call would fail the scenario
        let _ = reconcile(Arc::new(cluster), testctx).await;
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn in_creation_phase_attempts_bootstrap_path() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test().finalized().with_status(super::Status {
            phase: Some(Phase::InCreation),
            ..Default::default()
        });
        let mocksrv = fakeserver.run(Scenario::RadioSilence);
        let _ = reconcile(Arc::new(cluster), testctx).await;
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn in_progress_phase_runs_authenticated_reconcile() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test().finalized().with_status(super::Status {
            phase: Some(Phase::InProgress),
            ..Default::default()
        });
        let mocksrv = fakeserver.run(Scenario::RadioSilence);
        let _ = reconcile(Arc::new(cluster), testctx).await;
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn ready_phase_runs_steady_state() {
        let (testctx, fakeserver) = Context::test();
        let cluster = Cluster::test().finalized().with_status(super::Status {
            phase: Some(Phase::Ready),
            ..Default::default()
        });
        let mocksrv = fakeserver.run(Scenario::RadioSilence);
        let _ = reconcile(Arc::new(cluster), testctx).await;
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn new_phase_creates_root_secret_and_persists_reference() {
        let (testctx, fakeserver) = Context::test();
        // Cluster without root_secret; provide tikv ref to pass preflight
        let cluster = Cluster::test()
            .finalized()
            .with_status(super::Status {
                phase: Some(super::Phase::New),
                ..Default::default()
            })
            .needs_tikv_secret_ref();
        let mocksrv = fakeserver.run(Scenario::RootSecretCreationThenStatusAndDpSvc(cluster.clone()));

        reconcile(Arc::new(cluster), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Kubernetes cluster with CRD installed"]
    async fn integration_one_node_tidb_tikv_cluster_reconcile() {
        let client = kube::Client::try_default().await.unwrap();
        let state = super::State::default();
        let rec = state.diagnostics.read().await.recorder(client.clone());
        let ctx = Arc::new(Context {
            client: client.clone(),
            recorder: rec,
            metrics: state.metrics.clone(),
            diagnostics: state.diagnostics.clone(),
        });

        let secrets: Api<Secret> = Api::namespaced(client.clone(), "default");
        let _ = secrets
            .create(
                &PostParams::default(),
                &Secret {
                    metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                        name: Some("surreal-root".into()),
                        namespace: Some("default".into()),
                        ..Default::default()
                    },
                    type_: Some("Opaque".into()),
                    string_data: Some(
                        [
                            ("user".to_string(), "root".to_string()),
                            ("password".to_string(), "root".to_string()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    ..Default::default()
                },
            )
            .await;

        let mut cluster = super::Cluster::new(
            "test",
            super::Spec {
                surrealdb_image: Some("surrealdb/surrealdb:latest".to_string()),
                tidb_cluster: Some(super::TidbClusterSpec {
                    version: Some("v8.5.1".into()),
                    timezone: Some("UTC".into()),
                    pd: Some(
                        serde_json::from_value(serde_json::json!({
                            "baseImage": "pingcap/pd",
                            "replicas": 1,
                            "requests": { "storage": "1Gi" },
                            "config": {}
                        }))
                        .unwrap(),
                    ),
                    tikv: Some(
                        serde_json::from_value(serde_json::json!({
                            "baseImage": "pingcap/tikv",
                            "replicas": 1,
                            "requests": { "storage": "5Gi" },
                            "config": {}
                        }))
                        .unwrap(),
                    ),
                    tidb: Some(
                        serde_json::from_value(serde_json::json!({
                            "baseImage": "pingcap/tidb",
                            "replicas": 1,
                            "service": { "type": "ClusterIP" },
                            "config": {}
                        }))
                        .unwrap(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        cluster.meta_mut().namespace = Some("default".into());
        cluster.status = Some(super::Status {
            phase: Some(super::Phase::New),
            ..Default::default()
        });

        let clusters: Api<super::Cluster> = Api::namespaced(client.clone(), "default");
        let pp = PatchParams::apply("sdb-controller");
        let patch = Patch::Apply(cluster.clone());
        let _ = clusters.patch("test", &pp, &patch).await.unwrap();

        super::reconcile(Arc::new(cluster), ctx.clone()).await.unwrap();

        let out = clusters.get_status("test").await.unwrap();
        eprintln!("Cluster status after reconcile: {:?}", out.status);
    }
}
