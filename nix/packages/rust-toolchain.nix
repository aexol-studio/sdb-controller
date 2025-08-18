{
  inputs,
  system,
  pkgs,
  ...
}:
with inputs.fenix.packages.${system};
  combine ([
      latest.cargo
      latest.clippy
      latest.rustc
      latest.rustfmt
      latest.rust-analyzer
      latest.rust-src
    ]
    ++ (
      if pkgs.stdenv.hostPlatform.system == "x86_64-linux"
      then [inputs.fenix.packages.${system}.targets.x86_64-unknown-linux-musl.latest.rust-std]
      else [latest.rust-std]
    ))
