{
  inputs,
  rust-toolchain,
  pkgs,
  system,
  fetchFromGitHub,
  ...
}: let
  src = fetchFromGitHub {
    owner = "Dennor";
    repo = "kopium";
    rev = "da5cedfdea1d844171d67a1c2ccb20c1b94f7bd1";
    hash = "sha256-tC0EHQ7plZtF/eYkuKdzzJMtJ6xce+6pBmgsm3PfVeA=";
  };
  cargoInfo = builtins.fromTOML (builtins.readFile "${src}/Cargo.toml");
  craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rust-toolchain;
in
  craneLib.buildPackage ({
      name = cargoInfo.package.name;
      version = cargoInfo.package.version;
      src = craneLib.cleanCargoSource src;
      strictDeps = true;
      # Needs extra step for testing, skip for now
      doCheck = false;
    }
    // (
      if system == "x86_64-linux"
      then {
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      }
      else {}
    ))
