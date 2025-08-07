{
  stdenv,
  fetchurl,
}: let
  version = "1.6.3";
  crdYaml = fetchurl {
    url = "https://raw.githubusercontent.com/pingcap/tidb-operator/v${version}/manifests/crd.yaml";
    hash = "sha256-b/Q7zvkGXNHqT1oZWXLo5e5JC/07TlngO6UxRd/hUYU=";
  };
in
  stdenv.mkDerivation {
    inherit version;
    name = "tidb-crd";
    dontUnpack = true;
    installPhase = ''
      cp ${crdYaml} $out
    '';
  }
