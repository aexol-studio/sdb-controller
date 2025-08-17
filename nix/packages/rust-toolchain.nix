{
  inputs,
  system,
  pkgs,
  ...
}:
with inputs.fenix.packages.${system};
  combine ([
      stable.cargo
      stable.clippy
      stable.rustc
      stable.rustfmt
      stable.rust-analyzer
      stable.rust-src
    ]
    ++ (
      if pkgs.stdenv.hostPlatform.system == "x86_64-linux"
      then [inputs.fenix.packages.${system}.targets.x86_64-unknown-linux-musl.stable.rust-std]
      else [stable.rust-std]
    ))
