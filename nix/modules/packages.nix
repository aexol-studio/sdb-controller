{inputs, ...}: {
  imports = [
    inputs.pkgs-by-name-for-flake-parts.flakeModule
  ];
  perSystem = {
    system,
    config,
    withSystem,
    ...
  }: {
    pkgsDirectory = ../packages;
  };
}
