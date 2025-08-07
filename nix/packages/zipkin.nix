{
  lib,
  stdenv,
  fetchurl,
  makeWrapper,
  jre,
}:
stdenv.mkDerivation rec {
  version = "3.5.1";
  pname = "zipkin-server";
  src = fetchurl {
    url = "https://repo1.maven.org/maven2/io/zipkin/zipkin-server/${version}/zipkin-server-${version}-exec.jar";
    sha256 = "sha256-esYzpoMmR+ceUAEqXj6R+8cFRBSVUmFyRSYDD68nHY0=";
  };
  nativeBuildInputs = [makeWrapper];

  buildCommand = ''
    mkdir -p $out/share/java
    cp ${src} $out/share/java/zipkin-server-${version}-exec.jar
    mkdir -p $out/bin
    makeWrapper ${jre}/bin/java $out/bin/zipkin-server \
      --add-flags "-jar $out/share/java/zipkin-server-${version}-exec.jar"
  '';
  meta = with lib; {
    description = "Zipkin distributed tracing system";
    homepage = "https://zipkin.io/";
    sourceProvenance = with sourceTypes; [binaryBytecode];
    license = licenses.asl20;
    platforms = platforms.unix;
    mainProgram = "zipkin-server";
  };
}
