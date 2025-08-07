{
  pkgs,
  generate,
  ...
}:
with pkgs;
  writeShellApplication {
    name = "install-crd";
    runtimeInputs = [kubernetes-helm generate];
    text = ''
      set -e
      generate
      kubectl apply -f yaml/crd.yaml
    '';
  }
