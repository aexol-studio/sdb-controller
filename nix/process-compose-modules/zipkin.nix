{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.zipkin;
in
  with lib; {
    options.services.zipkin = {
      enable = mkEnableOption "Run zipkin";

      package = mkOption {
        type = types.package;
        description = "Which zipkin package to use.";
        default = pkgs.callPackage ../packages/zipkin.nix {};
        defaultText = lib.literalExpression "pkgs.callPackage ../packages/zipkin.nix {}";
      };
    };

    config = lib.mkIf cfg.enable {
      settings.processes.zipkin = {
        command = "${getExe cfg.package}";
        readiness_probe = {
          http_get = {
            host = "localhost";
            port = 9411;
            path = "/health";
          };
          initial_delay_seconds = 2;
          period_seconds = 10;
          timeout_seconds = 4;
          success_threshold = 1;
          failure_threshold = 5;
        };
      };
    };
  }
