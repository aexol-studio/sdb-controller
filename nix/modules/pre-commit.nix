{inputs, ...}: {
  imports = [inputs.git-hooks.flakeModule];
  perSystem = {
    system,
    pkgs,
    ...
  }: {
    pre-commit.settings = {
      hooks.treefmt = {
        enable = true;
      };
    };
  };
}
