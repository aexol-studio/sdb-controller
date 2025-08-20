use crate::{
    Result,
    user::crd::{ClusterRef, DatabaseRef, User, UserSpec, UserStatus},
};
use actix_web::body::MessageBody;
use http::{Request, Response, StatusCode};
use kube::{Client, Resource, ResourceExt, client::Body};
use std::sync::Arc;

// Test constructor helpers
impl User {
    pub fn finalized(mut self) -> Self {
        self.finalizers_mut()
            .push("users.surrealdb.aexol.com".to_string());
        self
    }
    pub fn test() -> Self {
        let mut u = User::new(
            "test-user",
            UserSpec {
                cluster_ref: ClusterRef {
                    name: "test".into(),
                    namespace: None,
                },
                database_ref: DatabaseRef {
                    name: "test-db".into(),
                    namespace: None,
                },
                username: None,
                password: None,
                secret_name: None,
            },
        );
        u.meta_mut().namespace = Some("default".into());
        u.meta_mut().uid = Some("2222-22-22-22-222222".into());
        u
    }
    pub fn with_status(mut self, status: UserStatus) -> Self {
        self.status = Some(status);
        self
    }
    pub fn with_username(mut self, name: &str) -> Self {
        self.spec.username = Some(name.into());
        self
    }
    pub fn with_password(mut self, pw: &str) -> Self {
        self.spec.password = Some(pw.into());
        self
    }
}

type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;
pub struct ApiServerVerifier(ApiServerHandle);

pub enum Scenario {
    FinalizerCreation(User),
    NewCreatesUserAndSecretAndConfigMapDefaultNames(User),
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
                Scenario::FinalizerCreation(u) => self.handle_finalizer_creation(u).await,
                Scenario::NewCreatesUserAndSecretAndConfigMapDefaultNames(u) => {
                    self.handle_get_database(&u)
                        .await
                        .unwrap()
                        .handle_get_cluster(&u, None)
                        .await
                        .unwrap()
                        .handle_get_secret_root("default")
                        .await
                        .unwrap()
                        .handle_proxy_define_user(&u, None)
                        .await
                        .unwrap()
                        .handle_create_secret(&u)
                        .await
                        .unwrap()
                        .handle_create_configmap(&u)
                        .await
                }
            }
            .expect("scenario completed without errors");
        })
    }

    async fn handle_get_database(mut self, user: &User) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::GET);
        assert!(req.uri().to_string().contains(&format!(
            "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{}/databases/{}",
            user.namespace().as_deref().unwrap_or("default"),
            user.spec.database_ref.name
        )));
        let body = serde_json::json!({
            "apiVersion":"surrealdb.aexol.com/v1alpha1","kind":"Database",
            "metadata": {"name": user.spec.database_ref.name, "namespace": user.namespace() },
            "spec": {"clusterRef": {"name": user.spec.cluster_ref.name}, "dbName": "app"}
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_get_cluster(mut self, user: &User, cluster_ns_override: Option<&str>) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let user_namespace = user.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            user.spec
                .cluster_ref
                .namespace
                .as_deref()
                .unwrap_or(user_namespace.as_deref().unwrap()),
        );
        assert_eq!(req.method(), http::Method::GET);
        assert!(req.uri().to_string().contains(&format!(
            "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{cluster_ns}/clusters/{}",
            user.spec.cluster_ref.name
        )));
        let body = serde_json::json!({
            "apiVersion":"surrealdb.aexol.com/v1alpha1","kind":"Cluster",
            "metadata": {"name": user.spec.cluster_ref.name, "namespace": cluster_ns },
            "spec": {"rootSecret": {"name":"surreal-root","namespace": cluster_ns}}
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
        assert!(
            req.uri()
                .to_string()
                .contains(&format!("/api/v1/namespaces/{cluster_ns}/secrets/surreal-root"))
        );
        let secret = serde_json::json!({
            "apiVersion":"v1","kind":"Secret",
            "metadata":{"name":"surreal-root","namespace": cluster_ns},
            "data":{"user":"cm9vdA==","password":"cm9vdA=="}
        });
        send.send_response(
            Response::builder()
                .body(Body::from(serde_json::to_vec(&secret).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_proxy_define_user(
        mut self,
        user: &User,
        cluster_ns_override: Option<&str>,
    ) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        let user_namespace = user.namespace();
        let cluster_ns = cluster_ns_override.unwrap_or(
            user.spec
                .cluster_ref
                .namespace
                .as_deref()
                .unwrap_or(user_namespace.as_deref().unwrap()),
        );
        let expected = format!(
            "/api/v1/namespaces/{}/services/{}-surrealdb:8000/proxy/sql",
            cluster_ns, user.spec.cluster_ref.name
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
            body.contains("DEFINE USER ") && body.contains("ON DATABASE"),
            "unexpected SQL: {body}"
        );
        assert!(
            !body.contains("GRANT ALL ON DB"),
            "unexpected GRANT clause present: {body}"
        );
        send.send_response(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("{}".try_into_bytes().unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_create_secret(mut self, user: &User) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::POST);
        assert!(req.uri().to_string().contains(&format!(
            "/api/v1/namespaces/{}/secrets?",
            user.namespace().unwrap()
        )));
        let body = read_json(req.into_body()).await;
        let meta = body.get("metadata").unwrap();
        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
        assert!(name.ends_with("-credentials"), "unexpected secret name: {name}");
        let resp = serde_json::json!({
            "apiVersion":"v1","kind":"Secret","metadata":{"name": name}
        });
        send.send_response(
            Response::builder()
                .status(StatusCode::CREATED)
                .body(Body::from(serde_json::to_vec(&resp).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_create_configmap(mut self, user: &User) -> Result<Self> {
        let (req, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(req.method(), http::Method::POST);
        assert!(req.uri().to_string().contains(&format!(
            "/api/v1/namespaces/{}/configmaps?",
            user.namespace().unwrap()
        )));
        let body = read_json(req.into_body()).await;
        let meta = body.get("metadata").unwrap();
        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
        assert!(name.ends_with("-config"), "unexpected configmap name: {name}");
        // check data keys
        let data = body
            .get("data")
            .and_then(|d| d.as_object())
            .expect("data present");
        assert!(data.contains_key("username"));
        assert!(data.contains_key("url"));
        let resp = serde_json::json!({
            "apiVersion":"v1","kind":"ConfigMap","metadata":{"name": name}
        });
        send.send_response(
            Response::builder()
                .status(StatusCode::CREATED)
                .body(Body::from(serde_json::to_vec(&resp).unwrap()))
                .unwrap(),
        );
        Ok(self)
    }

    async fn handle_finalizer_creation(mut self, u: User) -> Result<Self> {
        let (request, send) = self.0.next_request().await.expect("service not called");
        assert_eq!(request.method(), http::Method::PATCH);
        assert!(request.uri().to_string().contains(&format!(
            "/apis/surrealdb.aexol.com/v1alpha1/namespaces/{}/users/{}?",
            u.namespace().unwrap(),
            u.name_any()
        )));
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let runtime_patch: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid user from runtime");
        let ops = runtime_patch.as_array().expect("json patch array");
        assert!(
            ops.iter()
                .any(|op| op.get("op").and_then(|v| v.as_str()) == Some("add")
                    && op.get("path").and_then(|v| v.as_str()) == Some("/metadata/finalizers"))
        );
        let response = serde_json::to_vec(&u).unwrap();
        send.send_response(Response::builder().body(Body::from(response)).unwrap());
        Ok(self)
    }
}

pub struct UserContextMock;
impl UserContextMock {
    pub fn test() -> (Arc<crate::user::crd::UserContext>, ApiServerVerifier) {
        let (svc, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let client = Client::new(svc, "default");
        let service_client = crate::service::Client::new(client.clone());
        let ctx = crate::user::crd::UserContext {
            client,
            service_client,
        };
        (Arc::new(ctx), ApiServerVerifier(handle))
    }
}
