{
  pkgs,
  rust-toolchain,
  install-crd,
  tidb-crd,
  ...
}:
with pkgs;
  writeShellApplication {
    name = "set-dev-cluster";
    runtimeInputs = [rust-toolchain kubernetes-helm stdenv.cc kind install-crd];
    text = ''
      set -e
      export KIND_CLUSTER_NAME=sdb-controller-kind-cluster
      kind get kubeconfig >/dev/null 2>&1 || (
        kind create cluster
        kubectl create -f ${tidb-crd}
        helm repo add pingcap https://charts.pingcap.org
        helm repo update
        helm install \
        	-n tidb-operator \
          --create-namespace \
        	tidb-operator \
        	pingcap/tidb-operator \
        	--version ${tidb-crd.version}
      )
      kubectl config use-context kind-$KIND_CLUSTER_NAME
      install-crd
    '';
  }
