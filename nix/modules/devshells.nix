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
      shellHook =
        ''
          ${config.pre-commit.installationScript}
          ${config.packages.helix-config}/bin/helix-config
          # For rust-analyzer 'hover' tooltips to work.
          export RUST_SRC_PATH="${toolchain}/lib/rustlib/src/rust/library";
        ''
        + (
          if pkgs.stdenv.hostPlatform.system == "x86_64-linux"
          then ''
            export CARGO_BUILD_TARGET="x86_64-unknown-linux-musl"
            # opt-level=1 is here due to few weird issues, serde_json macros without
            # optimizations overflow the stack while deserializing huge json. On the other hand,
            # if opt-level is set in Cargo.toml suddenly mimalloc starts being linked against
            # glibc.
            export CARGO_INCREMENTAL="true"
            export CARGO_BUILD_RUSTFLAGS="-C target-feature=+crt-static -C opt-level=1 -C codegen-units=1024"
          ''
          else ''
          ''
        );
    };
  };
}
