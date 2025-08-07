{
  pkgs,
  rust-toolchain,
  install-crd,
  ...
}:
with pkgs;
  writeShellApplication {
    name = "run";
    runtimeInputs = [rust-toolchain stdenv.cc install-crd];
    text = ''
      set -e
      install-crd
      RUST_LOG=info,kube=debug,controller=debug cargo run
    '';
  }
