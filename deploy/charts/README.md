This directory contains the Helm chart for sdb-controller, intended for OCI
distribution via ghcr.io.

Publish (OCI):

```sh
# Package and push to GHCR (requires authenticated docker/gh)
export CHART=deploy/charts/sdb-controller
export VERSION=0.1.0
helm package $CHART --version $VERSION -d /tmp
helm push /tmp/sdb-controller-$VERSION.tgz oci://ghcr.io/aexol-studio/charts
```

Install (OCI):

```sh
helm upgrade --install sdb-controller oci://ghcr.io/aexol-studio/charts/sdb-controller \
  --namespace sdb-system --create-namespace
```

Optional TiDB controller-manager:

```sh
helm upgrade --install sdb-controller oci://ghcr.io/aexol-studio/charts/sdb-controller \
  -n sdb-system --create-namespace \
  --set tidbController.enabled=true
```
