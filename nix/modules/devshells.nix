{
  perSystem = {
    config,
    pkgs,
    lib,
    self',
    system,
    ...
  }: let
    toolchain = config.packages.rust-toolchain;
  in {
    devShells.default = pkgs.mkShell {
      name = "default";
      meta.description = "Rust development environment, created by rust-flake";
      buildInputs = [
        pkgs.libiconv
      ];
      packages = with pkgs; [
        stdenv.cc
        config.packages.rust-toolchain
        kubectl
        kubernetes-helm
        kind
      ];
      nativeBuildInputs = [config.treefmt.build.wrapper];
      shellHook = ''
        ${config.pre-commit.installationScript}
        ${config.packages.helix-config}/bin/helix-config
        # For rust-analyzer 'hover' tooltips to work.
        export RUST_SRC_PATH="${toolchain}/lib/rustlib/src/rust/library";
      '';
    };
  };
}
