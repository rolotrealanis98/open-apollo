{ lib, stdenv, kernel }:

stdenv.mkDerivation {
  pname = "ua-apollo";
  version = "1.2.0";

  src = ../driver;

  nativeBuildInputs = kernel.moduleBuildDependencies;

  buildPhase = ''
    make -C ${kernel.dev}/lib/modules/${kernel.modDirVersion}/build \
      M=$PWD modules
  '';

  installPhase = ''
    install -D ua_apollo.ko $out/lib/modules/${kernel.modDirVersion}/extra/ua_apollo.ko
  '';

  meta = with lib; {
    description = "Universal Audio Apollo Thunderbolt kernel driver";
    license = licenses.gpl2Only;
    platforms = platforms.linux;
  };
}
