{inputs, ...}: {
  imports = [inputs.treefmt-nix.flakeModule];
  perSystem = {
    system,
    pkgs,
    ...
  }: {
    treefmt = {
      projectRootFile = "flake.nix";
      programs = {
        alejandra.enable = true;
        deno.enable = true;
        rustfmt.enable = true;
      };
      settings = {
        formatter.rustfmt.options = ["--config-path" "${../../rustfmt.toml}"];
        formatter.deno.excludes = [".pre-commit-config.yaml" "deploy/charts/**"];
        global.excludes = [".envrc" "deploy/charts/sdb-controller/templates/**"];
      };
    };
  };
}
