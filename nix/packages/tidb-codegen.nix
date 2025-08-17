{
  writeShellApplication,
  yq,
  kopium,
  formats,
  stdenv,
  tidb-crd,
  ...
}: let
  toYaml = (formats.yaml {}).generate;
  crd = builtins.concatStringsSep "\n" (
    builtins.map
    (crd: let
      kind = builtins.elemAt (builtins.split "\\." crd.metadata.name) 0;
      file = "${kind}.yaml";
      rsFile = "tidb-api/src/tidb/${kind}.rs";
    in
      if kind == "tidbclusters"
      then "${kopium}/bin/kopium --derive Default --derive PartialEq  -Af - < ${toYaml "${file}" crd} > ${rsFile}  && chmod +w ${rsFile}"
      else "")
    (builtins.fromJSON "${builtins.readFile (stdenv.mkDerivation {
      name = "gen-json-crd";
      dontBuild = true;
      dontUnpack = true;
      buildInputs = [yq];
      installPhase = ''
        cat ${tidb-crd} | yq -s > $out
      '';
    })}")
  );
in
  writeShellApplication {
    name = "tidb-codegen";
    text = crd;
  }
