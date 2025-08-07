{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.controller;
in
  with lib; {
    options.services.controller = {
      enable = mkEnableOption "Run controller";

      package = mkOption {
        type = types.package;
        description = "Which controller package to use.";
        default = pkgs.writeShellApplication {
          name = "run-controller";
          runtimeInputs = [pkgs.watchexec];
          text = ''
            export RUST_LOG=info,kube=debug,controller=debug
            watchexec -r -e rs -- cargo run
          '';
        };
      };
    };

    config = lib.mkIf cfg.enable {
      settings.processes.controller = {
        command = "${getExe cfg.package}";
      };
    };
  }
