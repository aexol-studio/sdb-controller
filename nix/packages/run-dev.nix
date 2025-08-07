{
  pkgs,
  rust-toolchain,
  set-dev-cluster,
  ...
}:
with pkgs;
  writeShellApplication {
    name = "run-dev";
    runtimeInputs = [rust-toolchain stdenv.cc set-dev-cluster];
    text = ''
      set-dev-cluster
      RUST_LOG=info,kube=debug,controller=debug cargo run
    '';
  }
