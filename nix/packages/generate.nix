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
      cargo run --bin crdgen > yaml/crd.yaml
      helm template charts/sdb-controller > yaml/deployment.yaml
    '';
  }
