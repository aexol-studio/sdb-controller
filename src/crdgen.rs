use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    JSONSchemaProps, JSONSchemaPropsOrArray,
};
use kube::CustomResourceExt;
fn main() {
    let mut crd = controller::Cluster::crd();
    // fix missing x_kubernetes_preserve_unknown_fields values
    for v in &mut crd.spec.versions {
        let props = v
            .schema
            .as_mut()
            .expect("missing schema")
            .open_api_v3_schema
            .as_mut()
            .expect("missing openapi v3 schema")
            .properties
            .as_mut()
            .expect("missing openapi v3 props")
            .get_mut("spec")
            .expect("missing spec property")
            .properties
            .as_mut()
            .expect("missing openapi v3 props")
            .get_mut("tidbCluster")
            .expect("missing tidbCluster property")
            .properties
            .as_mut()
            .expect("missing tidbCluster properties");
        props.entry("pd".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("pdms".into()).and_modify(|v| {
            match v
                .items
                .get_or_insert_with(|| JSONSchemaPropsOrArray::Schema(Default::default()))
            {
                JSONSchemaPropsOrArray::Schema(schema) => {
                    schema.properties.get_or_insert_default().insert(
                        "config".into(),
                        JSONSchemaProps {
                            x_kubernetes_preserve_unknown_fields: Some(true),
                            ..Default::default()
                        },
                    );
                }
                _ => {}
            }
        });
        props.entry("pump".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("ticdc".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("tidb".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("tiflash".into()).and_modify(|v| {
            let props = v.properties.get_or_insert_default();
            props.insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
            props.insert(
                "proxy".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("tikv".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
        props.entry("tiproxy".into()).and_modify(|v| {
            v.properties.get_or_insert_default().insert(
                "config".into(),
                JSONSchemaProps {
                    x_kubernetes_preserve_unknown_fields: Some(true),
                    ..Default::default()
                },
            );
        });
    }
    print!("{}", serde_yaml::to_string(&crd).unwrap())
}
