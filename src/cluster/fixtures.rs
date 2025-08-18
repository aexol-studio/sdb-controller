//! Helper methods only available for tests
use crate::{CLUSTER_FINALIZER, Cluster, Context, Phase, Result, Spec, Status};
use actix_web::body::MessageBody;
use assert_json_diff::assert_json_include;
use http::{Request, Response, StatusCode};
use k8s_openapi::api::core::v1::{SecretKeySelector, SecretReference};
use kube::core::object::HasSpec;
use kube::{Client, Resource, ResourceExt, client::Body, runtime::events::Recorder};
use std::sync::Arc;

impl Cluster {
    /// A normal test cluster
    pub fn test() -> Self {
        let mut d = Cluster::new("test", Spec::default());
        d.meta_mut().namespace = Some("default".into());
        d.meta_mut().uid = Some("0000-00-00-00-000000".into());
        d
    }

    /// Modify cluster to set a deletion timestamp
    pub fn needs_delete(mut self) -> Self {
        use chrono::prelude::{DateTime, TimeZone, Utc};
        let now: DateTime<Utc> = Utc.with_ymd_and_hms(2017, 04, 02, 12, 50, 32).unwrap();
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
        self.meta_mut().deletion_timestamp = Some(Time(now));
        self
    }

    /// Modify cluster to set a set root credentials secret ref
    pub fn needs_root_credentials(mut self) -> Self {
        self.spec_mut().root_secret = Some(SecretReference {
            name: Some(String::from("test")),
            ..Default::default()
        });
        self
    }

    /// Modify cluster to set a set tikv secret ref
    pub fn needs_tikv_secret_ref(mut self) -> Self {
        self.spec_mut().tikv_secret_ref = Some(SecretKeySelector {
            key: String::from("test"),
            name: String::from("test"),
            ..Default::default()
        });
        self
    }

    /// Modify a cluster to have the expected finalizer
    pub fn finalized(mut self) -> Self {
        self.finalizers_mut().push(CLUSTER_FINALIZER.to_string());
        self
    }

    /// Modify a cluster to have an expected status
    pub fn with_status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self
    }
}

// We wrap tower_test::mock::Handle
type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;
pub struct ApiServerVerifier(ApiServerHandle);

/// Scenarios we test for in ApiServerVerifier
pub enum Scenario {
    /// objects without finalizers will get a finalizer applied (and not call the apply loop)
    FinalizerCreation(Cluster),
    /// finalized objects "with errors" (i.e. the "illegal" object) will short circuit the apply loop
    RadioSilence,
    /// objects with a deletion timestamp will run the cleanup loop sending event and removing the finalizer
    Cleanup(String, Cluster),
    StatusPatchThenDeploymentAndServiceCreation(Cluster),
    InCreationReadyThenBootstrapThenAuth(Cluster),
    DeploymentAndServiceWithAuthEnv(Cluster),
    RootSecretCreationThenStatusAndDpSvc(Cluster),
}

pub async fn timeout_after_1s(handle: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(1), handle)
        .await
        .expect("timeout on mock apiserver")
        .expect("scenario succeeded")
}

async fn read_json(body: Body) -> serde_json::Value {
    let bytes = body.collect_bytes().await.unwrap();
    serde_json::from_slice(&bytes).expect("valid json")
}

impl ApiServerVerifier {
    /// Tests only get to run specific scenarios that has matching handlers
    ///
    /// This setup makes it easy to handle multiple requests by chaining handlers together.
    ///
    /// NB: If the controller is making more calls than we are handling in the scenario,
    /// you then typically see a `KubeError(Service(Closed(())))` from the reconciler.
    ///
    /// You should await the `JoinHandle` (with a timeout) from this function to ensure that the
    /// scenario runs to completion (i.e. all expected calls were responded to),
    /// using the timeout to catch missing api calls to Kubernetes.
    pub fn run(self, scenario: Scenario) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // moving self => one scenario per test
            match scenario {
                Scenario::FinalizerCreation(cluster) => self.handle_finalizer_creation(cluster).await,
                Scenario::RadioSilence => Ok(self),
                Scenario::Cleanup(reason, cluster) => {
                    self.handle_get_owned_deployment(&cluster)
                        .await
                        .unwrap()
                        .handle_delete_owned_deployment(&cluster)
                        .await
                        .unwrap()
                        .handle_get_owned_service(&cluster)
                        .await
                        .unwrap()
                        .handle_delete_owned_service(&cluster)
                        .await
                        .unwrap()
                        .handle_get_owned_secret(&cluster)
                        .await
                        .unwrap()
                        .handle_delete_owned_secret(&cluster)
                        .await
                        .unwrap()
                        .handle_get_owned_tidbcluster(&cluster)
                        .await
                        .unwrap()
                        .handle_delete_owned_tidbcluster(&cluster)
                        .await
                        .unwrap()
                        .handle_event_create(reason)
                        .await
                        .unwrap()
                        .handle_finalizer_removal(cluster.clone())
                        .await
                }
                Scenario::StatusPatchThenDeploymentAndServiceCreation(cluster) => {
                    self.handle_status_patch_phase(cluster.clone(), "InCreation")
                        .await
                        .unwrap()
                        .handle_deployment_apply_with_env(&cluster, true)
                        .await
                        .unwrap()
                        .handle_service_apply(&cluster)
                        .await
                }
                Scenario::InCreationReadyThenBootstrapThenAuth(cluster) => {
                    self.handle_deployment_get_ready(&cluster)
                        .await
                        .unwrap()
                        .handle_secret_get_root(&cluster)
                        .await
                        .unwrap()
                        .handle_bootstrap_sql(&cluster)
                        .await
                        .unwrap()
                        .handle_status_patch_phase(cluster.clone(), "InProgress")
                        .await
                        .unwrap()
                        .handle_deployment_apply_with_env(&cluster, false)
                        .await
                        .unwrap()
                        .handle_service_apply(&cluster)
                        .await
                }
                Scenario::DeploymentAndServiceWithAuthEnv(cluster) => {
                    self.handle_deployment_apply_with_env(&cluster, false)
                        .await
                        .unwrap()
                        .handle_service_apply(&cluster)
                        .await
                }
                Scenario::RootSecretCreationThenStatusAndDpSvc(cluster) => {
                    self.handle_secret_create_auto(&cluster)
                        .await
                        .unwrap()
                        .handle_root_secret_spec_patch(&cluster)
                        .await
                        .unwrap()
                        .handle_status_patch_phase(cluster.clone(), "InCreation")
                        .await
                        .unwrap()
                        .handle_deployment_apply_with_env(&cluster, true)
                        .await
                        .unwrap()
                        .handle_service_apply(&cluster)
                        .await
                }
            }
            .expect("scenario completed without errors");
        })
    }

    // chainable scenario handlers

    async fn handle_finalizer_creation(mut self, cluster: Cluster) -> Result<Self> {
        let (request, send) = self.0.next_request().await.expect("service not called");
        // We expect a json patch to the specified cluster adding our finalizer
        assert_eq!(request.method(), http::Method::PATCH);
        assert_eq!(
            request.uri().to_string(),
            format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/default/clusters/{}?",
                cluster.name_any()
            )
        );
        let expected_patch = serde_json::json!([
            { "op": "test", "path": "/metadata/finalizers", "value": null },
            { "op": "add", "path": "/metadata/finalizers", "value": vec![CLUSTER_FINALIZER] }
        ]);
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let runtime_patch: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid cluster from runtime");
        assert_json_include!(actual: runtime_patch, expected: expected_patch);

        let response = serde_json::to_vec(&cluster.finalized()).unwrap(); // respond as the apiserver would have
        send.send_response(Response::builder().body(Body::from(response)).unwrap());
        Ok(self)
    }

    async fn handle_finalizer_removal(mut self, cluster: Cluster) -> Result<Self> {
        let (request, send) = self.0.next_request().await.expect("service not called");
        // We expect a json patch to the specified cluster removing our finalizer (at index 0)
        assert_eq!(request.method(), http::Method::PATCH);
        assert_eq!(
            request.uri().to_string(),
            format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/default/clusters/{}?",
                cluster.name_any()
            )
        );
        let expected_patch = serde_json::json!([
            { "op": "test", "path": "/metadata/finalizers/0", "value": CLUSTER_FINALIZER },
            { "op": "remove", "path": "/metadata/finalizers/0", "path": "/metadata/finalizers/0" }
        ]);
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let runtime_patch: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid cluster from runtime");
        assert_json_include!(actual: runtime_patch, expected: expected_patch);

        let response = serde_json::to_vec(&cluster).unwrap(); // respond as the apiserver would have
        send.send_response(Response::builder().body(Body::from(response)).unwrap());
        Ok(self)
    }

    async fn handle_event_create(mut self, reason: String) -> Result<Self> {
        let (request, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(request.method(), http::Method::POST);
        assert_eq!(
            request.uri().to_string(),
            format!("/apis/events.k8s.io/v1/namespaces/default/events?")
        );
        // verify the event reason matches the expected
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let postdata: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid event from runtime");
        dbg!("postdata for event: {}", postdata.clone());
        assert_eq!(
            postdata.get("reason").unwrap().as_str().map(String::from),
            Some(reason)
        );
        // then pass through the body
        send.send_response(Response::builder().body(Body::from(req_body)).unwrap());
        Ok(self)
    }

    async fn handle_get_owned_tidbcluster(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/pingcap.com/v1alpha1/namespaces/default/tidbclusters/{}",
                cluster.name_any()
            )),
            "unexpected tidbcluster get uri: {}",
            req.uri()
        );
        let body = serde_json::json!({
            "apiVersion": "pingcap.com/v1alpha1",
            "kind": "TidbCluster",
            "metadata": {
                "name": cluster.name_any(),
                "namespace": "default",
                "ownerReferences": [{
                    "apiVersion": Cluster::api_version(&()),
                    "kind": Cluster::kind(&()),
                    "name": cluster.name_any(),
                    "uid": "0000-00-00-00-000000"
                }]
            },
            "spec": {}
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_delete_owned_tidbcluster(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::DELETE);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/pingcap.com/v1alpha1/namespaces/default/tidbclusters/{}?",
                cluster.name_any()
            )),
            "unexpected tidbcluster delete uri: {}",
            req.uri()
        );
        let status = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success",
            "details": { "name": cluster.name_any(), "kind": "tidbclusters" }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&status).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    // Accept a status patch setting a phase by string (e.g., "InCreation", "InProgress")
    async fn handle_status_patch_phase(mut self, cluster: Cluster, phase: &str) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/default/clusters/{}/status?",
                cluster.name_any()
            )),
            "unexpected status patch uri: {}",
            req.uri()
        );

        // Body is a merge patch object like: { "status": { "phase": "<Phase>" } }
        let body = read_json(req.into_body()).await;
        let status = body.get("status").expect("status present in merge patch");
        let phase_val = status.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(phase_val, phase, "phase patch mismatch");

        // respond with echo (cluster + patched status)
        let mut echo = cluster.clone();
        echo.status = Some(Status {
            phase: match phase {
                "New" => Some(Phase::New),
                "Preparing" => Some(Phase::Preparing),
                "InCreation" => Some(Phase::InCreation),
                "InProgress" => Some(Phase::InProgress),
                "NotReady" => Some(Phase::NotReady),
                "Ready" => Some(Phase::Ready),
                "Error" => Some(Phase::Error),
                _ => Some(Phase::InProgress),
            },
            ..Default::default()
        });
        let resp = serde_json::to_vec(&echo).unwrap();
        send.send_response(Response::builder().body(Body::from(resp)).unwrap());
        Ok(self)
    }

    // Accept server-side apply of Deployment and validate env
    async fn handle_deployment_apply_with_env(
        mut self,
        cluster: &Cluster,
        creation_mode: bool,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/apps/v1/namespaces/default/deployments/{}-surrealdb?",
                cluster.name_any()
            )),
            "unexpected deployment apply uri: {}",
            req.uri()
        );
        let json = read_json(req.into_body()).await;
        // Find container env
        let env = json["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has = |name: &str| {
            env.iter()
                .any(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
        };
        let has_kv = |name: &str, val: &str| {
            env.iter().any(|e| {
                e.get("name").and_then(|n| n.as_str()) == Some(name)
                    && e.get("value").and_then(|v| v.as_str()) == Some(val)
            })
        };

        if creation_mode {
            assert!(
                has_kv("SURREAL_UNAUTHENTICATED", "true"),
                "creation mode requires SURREAL_UNAUTHENTICATED=true"
            );
            assert!(!has("SURREAL_AUTH"), "creation mode must not set SURREAL_AUTH");
        } else {
            assert!(
                has_kv("SURREAL_AUTH", "true"),
                "auth mode requires SURREAL_AUTH=true"
            );
            assert!(has("SURREAL_PATH"), "auth mode should set SURREAL_PATH");
            assert!(
                !has("SURREAL_UNAUTHENTICATED"),
                "auth mode must not set SURREAL_UNAUTHENTICATED"
            );
        }

        // echo back deployment (no need for full object)
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&json).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    // Accept server-side apply of Service for {name}-surrealdb
    async fn handle_service_apply(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(
            req.uri().to_string().contains(&format!(
                "/api/v1/namespaces/default/services/{}-surrealdb?",
                cluster.name_any()
            )),
            "unexpected service apply uri: {}",
            req.uri()
        );
        let json = read_json(req.into_body()).await;
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&json).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    // Simulate a GET deployment returning availableReplicas > 0
    async fn handle_deployment_get_ready(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/apps/v1/namespaces/default/deployments/{}-surrealdb",
                cluster.name_any()
            )),
            "unexpected deployment get uri: {}",
            req.uri()
        );
        let ready = serde_json::json!({
            "apiVersion":"apps/v1",
            "kind":"Deployment",
            "metadata": { "name": format!("{}-surrealdb", cluster.name_any()), "namespace":"default" },
            "status": { "availableReplicas": 1 }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&ready).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_secret_get_root(mut self, _cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(
            req.uri()
                .to_string()
                .contains("/api/v1/namespaces/default/secrets/test"),
            "unexpected secret get uri: {}",
            req.uri()
        );

        // "root" -> "cm9vdA=="
        let secret = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": "test",
                "namespace": "default"
            },
            "type": "Opaque",
            "data": {
                "user": "cm9vdA==",
                "password": "cm9vdA=="
            }
        });

        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&secret).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_bootstrap_sql(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let name = cluster.name_any();
        let ns = cluster.namespace().unwrap_or_default();
        let expected_path = format!("/api/v1/namespaces/{ns}/services/{name}-surrealdb:8000/proxy/sql");
        // Method and path
        assert_eq!(req.method(), &http::Method::POST, "bootstrap method must be POST");
        assert_eq!(req.uri().path(), expected_path, "bootstrap path mismatch");

        // Headers
        let ct = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(ct, "text/plain", "bootstrap content-type must be text/plain");

        // Body contains SQL
        let body = String::from_utf8(req.into_body().collect_bytes().await.unwrap().into()).unwrap();
        assert!(
            body.contains("DEFINE USER") && body.contains("PASSWORD"),
            "bootstrap SQL body must define user and password, got: {body}"
        );

        send.send_response(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("{}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_secret_create_auto(mut self, _cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::POST);
        assert!(
            req.uri()
                .to_string()
                .contains("/api/v1/namespaces/default/secrets?"),
            "unexpected secret create uri: {}",
            req.uri()
        );
        let body = read_json(req.into_body()).await;
        let meta = body.get("metadata").expect("secret.metadata");
        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        assert_eq!(name, "test-surrealdb-root", "auto secret name mismatch");
        // Echo back created Secret
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }
    async fn handle_root_secret_spec_patch(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/default/clusters/{}?",
                cluster.name_any()
            )),
            "unexpected cluster patch uri: {}",
            req.uri()
        );
        let patch = read_json(req.into_body()).await;
        let ops = patch.as_array().expect("json patch array");
        // Find replace op on /spec/rootSecret
        let rs = ops
            .iter()
            .find_map(|op| {
                (op.get("op").and_then(|v| v.as_str()) == Some("replace")
                    && op.get("path").and_then(|v| v.as_str()) == Some("/spec/rootSecret"))
                .then(|| op.get("value"))
                .flatten()
            })
            .expect("replace /spec/rootSecret present");
        assert_eq!(
            rs.get("name").and_then(|v| v.as_str()),
            Some("test-surrealdb-root")
        );
        assert_eq!(rs.get("namespace").and_then(|v| v.as_str()), Some("default"));

        // Echo back (don’t mutate spec)
        let resp = serde_json::to_vec(&cluster.clone()).unwrap();
        send.send_response(Response::builder().body(Body::from(resp)).unwrap());
        Ok(self)
    }
    async fn handle_delete_owned_deployment(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::DELETE);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/apps/v1/namespaces/default/deployments/{}-surrealdb?",
                cluster.name_any()
            )),
            "unexpected deployment delete uri: {}",
            req.uri()
        );
        let status = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success",
            "details": {
                "name": format!("{}-surrealdb", cluster.name_any()),
                "kind": "deployments"
            }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&status).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_delete_owned_service(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::DELETE);
        assert!(
            req.uri().to_string().contains(&format!(
                "/api/v1/namespaces/default/services/{}-surrealdb?",
                cluster.name_any()
            )),
            "unexpected service delete uri: {}",
            req.uri()
        );
        let status = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success",
            "details": {
                "name": format!("{}-surrealdb", cluster.name_any()),
                "kind": "services"
            }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&status).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_delete_owned_secret(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::DELETE);
        assert!(
            req.uri().to_string().contains(&format!(
                "/api/v1/namespaces/default/secrets/{}-surrealdb-root?",
                cluster.name_any()
            )),
            "unexpected secret delete uri: {}",
            req.uri()
        );
        let status = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Success",
            "details": {
                "name": format!("{}-surrealdb-root", cluster.name_any()),
                "kind": "secrets"
            }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&status).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_get_owned_service(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(
            req.uri().to_string().contains(&format!(
                "/api/v1/namespaces/default/services/{}-surrealdb",
                cluster.name_any()
            )),
            "unexpected service get uri: {}",
            req.uri()
        );
        let body = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": format!("{}-surrealdb", cluster.name_any()),
                "namespace": "default",
                "ownerReferences": [{
                    "apiVersion": Cluster::api_version(&()),
                    "kind": Cluster::kind(&()),
                    "name": cluster.name_any(),
                    "uid": "0000-00-00-00-000000"
                }]
            },
            "spec": { "ports": [], "selector": {} }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_get_owned_secret(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(
            req.uri().to_string().contains(&format!(
                "/api/v1/namespaces/default/secrets/{}-surrealdb-root",
                cluster.name_any()
            )),
            "unexpected secret get uri: {}",
            req.uri()
        );
        let body = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": format!("{}-surrealdb-root", cluster.name_any()),
                "namespace": "default",
                "ownerReferences": [{
                    "apiVersion": Cluster::api_version(&()),
                    "kind": Cluster::kind(&()),
                    "name": cluster.name_any(),
                    "uid": "0000-00-00-00-000000"
                }]
            },
            "type": "Opaque",
            "data": {} // content not needed for ownership check
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_get_owned_deployment(mut self, cluster: &Cluster) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(req.uri().to_string().contains(&format!(
            "/apis/apps/v1/namespaces/default/deployments/{}-surrealdb",
            cluster.name_any()
        )));
        let body = serde_json::json!({
            "apiVersion":"apps/v1","kind":"Deployment",
            "metadata": {
                "name": format!("{}-surrealdb", cluster.name_any()),
                "namespace":"default",
                "ownerReferences":[{
                    "apiVersion": Cluster::api_version(&()),
                    "kind": Cluster::kind(&()),
                    "name": cluster.name_any(),
                    "uid":"0000-00-00-00-000000"
                }]
            }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }
    pub fn proxy_bootstrap_post_ok(_name: &str, _ns: &str) -> http::Response<String> {
        http::Response::builder().status(200).body("{}".into()).unwrap()
    }
}

impl Context {
    // Create a test context with a mocked kube client, locally registered metrics and default diagnostics
    pub fn test() -> (Arc<Self>, ApiServerVerifier) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mock_client = Client::new(mock_service, "default");
        let mock_recorder = Recorder::new(mock_client.clone(), "cluster-ctrl-test".into());
        let ctx = Self {
            client: mock_client,
            metrics: Arc::default(),
            diagnostics: Arc::default(),
            recorder: mock_recorder,
        };
        (Arc::new(ctx), ApiServerVerifier(handle))
    }
}
