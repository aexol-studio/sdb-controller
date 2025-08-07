{
  inputs,
  system,
  ...
}:
# Macros like panic embed /nix/store paths into the resulting binary if there's
# source present. As such exclude rust-src from toolchain to avoid pulling it into
# /nix/store and increasing amount of dependencies.
with inputs.fenix.packages.${system};
  combine [
    stable.cargo
    stable.clippy
    stable.rustc
    stable.rustfmt
    stable.rust-analyzer
    stable.rust-src
    inputs.fenix.packages.${system}.targets.x86_64-unknown-linux-musl.stable.rust-std
  ]
