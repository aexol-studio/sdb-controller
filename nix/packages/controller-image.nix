{
  pkgs,
  controller,
  stdenv,
  ...
}: let
  cargoInfo = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
  controller' = stdenv.mkDerivation {
    name = "controller";
    dontBuild = true;
    dontUnpack = true;
    dontCheck = true;
    installPhase = ''
      mkdir -p $out/bin
      cp ${controller}/bin/controller $out/bin
    '';
  };
in
  pkgs.dockerTools.buildLayeredImage {
    name = cargoInfo.package.name;
    tag = cargoInfo.package.version;
    contents = with pkgs; [cacert];
    config.Entrypoint = ["${controller'}/bin/controller"];
  }
