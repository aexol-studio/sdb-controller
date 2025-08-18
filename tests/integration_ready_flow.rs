use kube::api::{Patch, PatchParams, PostParams};
use kube::{Api, Client, Resource};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
#[ignore]
async fn integration_cluster_then_database_becomes_ready() {
    let client = Client::try_default().await.expect("kube client");

    let secrets: Api<k8s_openapi::api::core::v1::Secret> = Api::namespaced(client.clone(), "default");
    let _ = secrets
        .create(
            &PostParams::default(),
            &k8s_openapi::api::core::v1::Secret {
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

    let clusters: Api<controller::Cluster> = Api::namespaced(client.clone(), "default");
    let mut cluster = controller::Cluster::new(
        "test",
        controller::Spec {
            surrealdb_image: Some("surrealdb/surrealdb:latest".to_string()),
            tidb_cluster: Some(tidb_api::tidbclusters::TidbClusterSpec {
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
                        "replicas": 0,
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
    cluster.status = Some(controller::Status {
        phase: Some(controller::Phase::New),
        ..Default::default()
    });

    let pp = PatchParams::apply("sdb-controller");
    let patch = Patch::Apply(cluster.clone());
    let _ = clusters.patch("test", &pp, &patch).await.expect("apply cluster");

    for _ in 0..60 {
        // 60 * 5s = 300s
        if let Ok(obj) = clusters.get("test").await {
            if obj.status.as_ref().and_then(|s| s.phase.clone()) == Some(controller::Phase::Ready) {
                break;
            }
        }
        sleep(Duration::from_secs(5)).await;
    }

    // Create Database that references Cluster "test"
    let dbs: Api<controller::database::crd::Database> = Api::namespaced(client.clone(), "default");
    let mut db = controller::database::crd::Database::new(
        "it-db",
        controller::database::crd::DbSpec {
            cluster_ref: controller::database::crd::ClusterRef {
                name: "test".into(),
                namespace: None,
            },
            db_namespace: None,
            db_name: Some("app".into()),
        },
    );
    db.meta_mut().namespace = Some("default".into());
    let _ = dbs
        .create(&PostParams::default(), &db)
        .await
        .expect("create database");

    for _ in 0..60 {
        // 60 * 2s = 120s
        if let Ok(obj) = dbs.get("it-db").await {
            if obj.status.as_ref().and_then(|s| s.phase.clone())
                == Some(controller::database::crd::DbPhase::Ready)
            {
                // cleanup
                let _ = dbs.delete("it-db", &Default::default()).await;
                let _ = clusters.delete("test", &Default::default()).await;
                return;
            }
        }
        sleep(Duration::from_secs(2)).await;
    }

    let _ = dbs.delete("it-db", &Default::default()).await;
    let _ = clusters.delete("test", &Default::default()).await;
    panic!("Database did not become Ready in time");
}
