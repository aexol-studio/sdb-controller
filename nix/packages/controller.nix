{
  inputs,
  rust-toolchain,
  pkgs,
  ...
}: let
  cargoInfo = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
  craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rust-toolchain;
in
  craneLib.buildPackage {
    name = cargoInfo.package.name;
    version = cargoInfo.package.version;

    src = craneLib.cleanCargoSource ../..;
    strictDeps = true;
    CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
    CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
  }
