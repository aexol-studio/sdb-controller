{pkgs, ...}:
with pkgs;
  writeShellApplication {
    name = "delete-dev-cluster";
    runtimeInputs = [kind];
    text = ''
      set -e
      export KIND_CLUSTER_NAME=sdb-controller-kind-cluster
      kind delete cluster
    '';
  }
