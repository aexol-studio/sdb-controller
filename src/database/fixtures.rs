use crate::{
    Result,
    database::crd::{ClusterRef, Database, DbPhase, DbSpec, DbStatus},
};
use actix_web::body::MessageBody;
use http::{Request, Response, StatusCode};
use kube::{Client, Resource, ResourceExt, client::Body, runtime::events::Recorder};
use std::sync::Arc;

// Test constructor
impl Database {
    pub fn finalized(mut self) -> Self {
        self.finalizers_mut()
            .push("databases.surrealdb.aexol.com".to_string());
        self
    }
    pub fn test() -> Self {
        let mut d = Database::new(
            "test-db",
            DbSpec {
                cluster_ref: ClusterRef {
                    name: "test".into(),
                    namespace: None,
                },
                db_namespace: None,
                db_name: Some("app".into()),
            },
        );
        d.meta_mut().namespace = Some("default".into());
        d.meta_mut().uid = Some("1111-11-11-11-111111".into());
        d
    }

    pub fn with_status(mut self, status: DbStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_cluster_ref_ns(mut self, ns: &str) -> Self {
        self.spec.cluster_ref.namespace = Some(ns.to_string());
        self
    }

    pub fn with_db_namespace(mut self, ns: &str) -> Self {
        self.spec.db_namespace = Some(ns.to_string());
        self
    }
}

// Mock server wrapper
type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;
pub struct ApiServerVerifier(ApiServerHandle);

// Scenarios for Database controller
pub enum Scenario {
    NewEnsuresNamespaceAndDatabase(Database),
    NewCrossNsEnsures(Database),
    NamespaceDefineError(Database),
    DatabaseDefineError(Database),
    FinalizerCreation(Database),
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
    pub fn run(self, scenario: Scenario) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            match scenario {
                Scenario::NewEnsuresNamespaceAndDatabase(db) => {
                    // Assume finalizer already present
                    self.handle_get_cluster(&db, None)
                        .await
                        .unwrap()
                        .handle_get_secret_root("default")
                        .await
                        .unwrap()
                        .handle_proxy_define_namespace(&db, None)
                        .await
                        .unwrap()
                        .handle_proxy_define_database(&db, None)
                        .await
                        .unwrap()
                        .handle_status_patch_phase(db.clone(), "Ready")
                        .await
                }
                Scenario::NewCrossNsEnsures(db) => {
                    let cluster_ns = db.spec.cluster_ref.namespace.as_deref().unwrap_or("default");
                    self.handle_get_cluster(&db, None)
                        .await
                        .unwrap()
                        .handle_get_secret_root(cluster_ns)
                        .await
                        .unwrap()
                        .handle_proxy_define_namespace(&db, Some(cluster_ns))
                        .await
                        .unwrap()
                        .handle_proxy_define_database(&db, Some(cluster_ns))
                        .await
                        .unwrap()
                        .handle_status_patch_phase(db.clone(), "Ready")
                        .await
                }
                Scenario::NamespaceDefineError(db) => {
                    self.handle_get_cluster(&db, None)
                        .await
                        .unwrap()
                        .handle_get_secret_root("default")
                        .await
                        .unwrap()
                        .handle_proxy_define_namespace_error(&db, None)
                        .await
                }
                Scenario::DatabaseDefineError(db) => {
                    self.handle_get_cluster(&db, None)
                        .await
                        .unwrap()
                        .handle_get_secret_root("default")
                        .await
                        .unwrap()
                        .handle_proxy_define_namespace(&db, None)
                        .await
                        .unwrap()
                        .handle_proxy_define_database_error(&db, None)
                        .await
                }
                Scenario::FinalizerCreation(db) => self.handle_finalizer_creation(db).await,
            }
            .expect("scenario completed without errors");
        })
    }

    async fn handle_get_cluster(mut self, db: &Database, cluster_ns_override: Option<&str>) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let db_namespace = db.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            db.spec
                .cluster_ref
                .namespace
                .as_ref()
                .unwrap_or(db_namespace.as_ref().unwrap()),
        );
        assert_eq!(req.method(), http::Method::GET);
        assert!(req.uri().to_string().contains(&format!(
            "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{cluster_ns}/clusters/{name}",
            name = db.spec.cluster_ref.name
        )));
        // respond with a Cluster that has spec.rootSecret.name="surreal-root"
        let body = serde_json::json!({
            "apiVersion": "surrealdb.aexol.com/v1alpha1",
            "kind": "Cluster",
            "metadata": { "name": db.spec.cluster_ref.name, "namespace": cluster_ns },
            "spec": { "rootSecret": { "name": "surreal-root", "namespace": cluster_ns } }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_get_secret_root(mut self, cluster_ns: &str) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        println!("{}", req.uri().to_string());
        assert!(
            req.uri()
                .to_string()
                .contains(&format!("/api/v1/namespaces/{cluster_ns}/secrets/surreal-root"))
        );
        // "root" => "cm9vdA=="
        let secret = serde_json::json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": { "name": "surreal-root", "namespace": cluster_ns },
            "data": { "user": "cm9vdA==", "password": "cm9vdA==" }
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&secret).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    // Handle adding our finalizer to the Database (json-patch add to metadata.finalizers)
    async fn handle_finalizer_creation(mut self, db: Database) -> Result<Self> {
        let (request, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(request.method(), http::Method::PATCH);
        assert_eq!(
            request.uri().to_string(),
            format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{}/databases/{}?",
                db.namespace().unwrap_or_default(),
                db.name_any()
            )
        );
        // Accept any json-patch that adds our finalizer array
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let runtime_patch: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid db from runtime");
        // Must contain an add on /metadata/finalizers
        let ops = runtime_patch.as_array().expect("json patch array");
        assert!(
            ops.iter()
                .any(|op| op.get("op").and_then(|v| v.as_str()) == Some("add")
                    && op.get("path").and_then(|v| v.as_str()) == Some("/metadata/finalizers")),
            "missing finalizer add op"
        );

        let response = serde_json::to_vec(&db).unwrap();
        send.send_response(Response::builder().body(Body::from(response)).unwrap());
        Ok(self)
    }

    // Accept a status merge-patch setting a phase
    async fn handle_status_patch_phase(mut self, db: Database, phase: &str) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(
            req.uri().to_string().contains(&format!(
                "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{}/databases/{}/status?",
                db.namespace().unwrap_or_default(),
                db.name_any()
            )),
            "unexpected status patch uri: {}",
            req.uri()
        );
        // merge patch: { "status": { "phase": "<Phase>" } }
        let body = read_json(req.into_body()).await;
        let status = body.get("status").expect("status present");
        let phase_val = status.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(phase_val, phase);

        // echo minimal object back
        let mut echo = db.clone();
        echo.status = Some(DbStatus {
            phase: Some(match phase {
                "New" => DbPhase::New,
                "InProgress" => DbPhase::InProgress,
                "Ready" => DbPhase::Ready,
                "Error" => DbPhase::Error,
                _ => DbPhase::InProgress,
            }),
            message: None,
        });
        let resp = serde_json::to_vec(&echo).unwrap();
        send.send_response(Response::builder().body(Body::from(resp)).unwrap());
        Ok(self)
    }

    // Helpers to assert proxy SQL calls

    async fn handle_proxy_define_namespace(
        mut self,
        db: &Database,
        cluster_ns_override: Option<&str>,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let db_namespace = db.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            db.spec
                .cluster_ref
                .namespace
                .as_ref()
                .unwrap_or(db_namespace.as_ref().unwrap()),
        );
        let expected = format!(
            "/api/v1/namespaces/{}/services/{}-surrealdb:8000/proxy/sql",
            cluster_ns, db.spec.cluster_ref.name
        );
        assert_eq!(req.method(), &http::Method::POST);
        assert_eq!(req.uri().path(), expected, "proxy path mismatch");
        let ct = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(ct, "text/plain");

        let body = String::from_utf8(req.into_body().collect_bytes().await.unwrap().into()).unwrap();
        assert!(
            body.starts_with("DEFINE NAMESPACE "),
            "expected DEFINE NAMESPACE, got: {body}"
        );

        send.send_response(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("{}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_proxy_define_namespace_error(
        mut self,
        db: &Database,
        cluster_ns_override: Option<&str>,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let db_namespace = db.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            db.spec
                .cluster_ref
                .namespace
                .as_ref()
                .unwrap_or(db_namespace.as_ref().unwrap()),
        );
        let expected = format!(
            "/api/v1/namespaces/{}/services/{}-surrealdb:8000/proxy/sql",
            cluster_ns, db.spec.cluster_ref.name
        );
        assert_eq!(req.method(), &http::Method::POST);
        assert_eq!(req.uri().path(), expected, "proxy path mismatch");

        send.send_response(
            Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("{\"err\":\"bad ns\"}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_proxy_define_database(
        mut self,
        db: &Database,
        cluster_ns_override: Option<&str>,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let db_namespace = db.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            db.spec
                .cluster_ref
                .namespace
                .as_ref()
                .unwrap_or(db_namespace.as_ref().unwrap()),
        );
        let expected = format!(
            "/api/v1/namespaces/{}/services/{}-surrealdb:8000/proxy/sql",
            cluster_ns, db.spec.cluster_ref.name
        );
        assert_eq!(req.method(), &http::Method::POST);
        assert_eq!(req.uri().path(), expected, "proxy path mismatch");
        let body = String::from_utf8(req.into_body().collect_bytes().await.unwrap().into()).unwrap();
        assert!(
            body.contains("USE NS `") && body.contains("DEFINE DATABASE `"),
            "expected quoted USE NS and DEFINE DATABASE, got: {body}"
        );
        send.send_response(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("{}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_proxy_define_database_error(
        mut self,
        db: &Database,
        cluster_ns_override: Option<&str>,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let db_namespace = db.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            db.spec
                .cluster_ref
                .namespace
                .as_ref()
                .unwrap_or(db_namespace.as_ref().unwrap()),
        );
        let expected = format!(
            "/api/v1/namespaces/{}/services/{}-surrealdb:8000/proxy/sql",
            cluster_ns, db.spec.cluster_ref.name
        );
        assert_eq!(req.method(), &http::Method::POST);
        assert_eq!(req.uri().path(), expected, "proxy path mismatch");

        send.send_response(
            Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("{\"err\":\"bad db\"}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }
}

pub struct DbContextMock;

impl DbContextMock {
    // Create a test context with a mocked kube client for Database controller
    pub fn test() -> (Arc<crate::database::crd::DbContext>, ApiServerVerifier) {
        let (svc, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let client = Client::new(svc, "default");
        let rec = Recorder::new(client.clone(), "database-ctrl-test".into());
        let ctx = crate::database::crd::DbContext {
            client,
            recorder: rec,
        };
        (Arc::new(ctx), ApiServerVerifier(handle))
    }
}
