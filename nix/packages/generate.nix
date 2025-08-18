{
  pkgs,
  rust-toolchain,
  ...
}:
with pkgs;
  writeShellApplication {
    name = "generate";
    runtimeInputs = [rust-toolchain kubernetes-helm stdenv.cc];
    text = ''
      set -e
      cargo run --bin crdgen | tee yaml/crd.yaml deploy/charts/sdb-controller/crds/crd.yaml >/dev/null
      helm template deploy/charts/sdb-controller > yaml/deployment.yaml
    '';
  }
