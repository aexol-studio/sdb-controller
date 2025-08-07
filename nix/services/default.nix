args @ {lib, ...}: let
  services = with builtins;
    lib.lists.foldl (
      acc: val: (lib.attrsets.recursiveUpdate acc val)
    )
    {}
    (
      map
      (path: (import path) args)
      (filter (path: baseNameOf path != "default.nix") (map (fn: ./${fn}) (attrNames (readDir ./.))))
    );
in {inherit services;}
