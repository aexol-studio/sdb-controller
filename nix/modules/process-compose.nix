args @ {inputs, ...}: {
  imports = [inputs.process-compose-flake.flakeModule];
  perSystem = {
    process-compose."default" = {config, ...}: ({
        imports = with builtins;
          [
            inputs.services-flake.processComposeModules.default
          ]
          ++ map (fn: import ../process-compose-modules/${fn}) (attrNames (readDir ../process-compose-modules));
      }
      // (import ../services args));
  };
}
