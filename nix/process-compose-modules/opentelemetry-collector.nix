{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.opentelemetry-collector;
in
  with lib; {
    options.services.opentelemetry-collector = {
      enable = mkEnableOption "Run opentelemetry-collector";

      package = mkOption {
        type = types.package;
        description = "Which opentelemetry-collector package to use.";
        default = pkgs.opentelemetry-collector;
        defaultText = lib.literalExpression "pkgs.opentelemetry-collector";
      };
    };

    config = lib.mkIf cfg.enable {
      settings.processes.opentelemetry-collector = {
        command = "${getExe cfg.package} --set=receivers.otlp.protocols.grpc.endpoint=0.0.0.0:4317 --set=exporters.zipkin.endpoint=http://zipkin:9411/api/v2/spans --set=service.pipelines.traces.receivers=[otlp] --set=service.pipelines.traces.exporters=[zipkin]";
        depends_on = {
          zipkin = {
            condition = "process_healthy";
          };
        };
      };
    };
  }
