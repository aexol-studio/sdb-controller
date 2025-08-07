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
      kubectl create -f yaml/crd.yaml
    '';
  }
