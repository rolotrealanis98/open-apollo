# uaconsole: the native mixer console (console-rs/), a Rust rebuild of UA's
# UAD Console for open-apollo.
#
# Nothing from Universal Audio is in this package. uaconsole reads UA's art,
# layouts and Appearance.json at run time from the user's own UAD Console
# install (see console-rs/README.md, "Where the art comes from").
{
  lib,
  stdenvNoCC,
  rustPlatform,
  fetchFromGitHub,
  makeWrapper,
  writeText,
  # Loaded at run time with dlopen by wgpu (Vulkan, then GL/EGL) and winit
  # (Wayland, then X11), so they are not link-time inputs.
  vulkan-loader,
  libGL,
  wayland,
  libxkbcommon,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  libxcb,
  fontconfig,
  liberation_ttf,
}:

let
  # Console's typefaces map to metric-compatible faces (console-rs/crates/gfx/src/text.rs):
  # Helvetica -> "Nimbus Sans", Arial -> "Liberation Sans". The family is matched by
  # name, so TeX Gyre Heros (gyre-fonts) does not stand in for Nimbus Sans, and nixpkgs
  # only ships the old Type 1 "Nimbus Sans L" (ghostscript.fonts), which fontdb cannot
  # load. Take the OpenType Nimbus Sans faces from URW's base35 release.
  nimbus-sans = stdenvNoCC.mkDerivation {
    pname = "urw-base35-nimbus-sans";
    version = "20200910";
    src = fetchFromGitHub {
      owner = "ArtifexSoftware";
      repo = "urw-base35-fonts";
      rev = "20200910";
      hash = "sha256-YQl5IDtodcbTV3D6vtJi7CwxVtHHl58fG6qCAoSaP4U=";
    };
    dontConfigure = true;
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      install -Dm644 -t $out/share/fonts/opentype fonts/NimbusSans-*.otf
      install -Dm644 -t $out/share/licenses/urw-base35-nimbus-sans COPYING LICENSE
      runHook postInstall
    '';
    meta = {
      description = "Nimbus Sans (URW++ base35, OpenType)";
      homepage = "https://github.com/ArtifexSoftware/urw-base35-fonts";
      # AGPL-3.0 with a font-embedding exception (see LICENSE in the source).
      license = lib.licenses.agpl3Only;
      platforms = lib.platforms.all;
    };
  };

  # fontdb (via cosmic-text) parses fontconfig files itself, without libfontconfig.
  # Point it at the two faces above, and keep the host's fonts too.
  fontsConf = writeText "uaconsole-fonts.conf" ''
    <?xml version="1.0"?>
    <!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
    <fontconfig>
      <dir>${nimbus-sans}/share/fonts</dir>
      <dir>${liberation_ttf}/share/fonts</dir>
      <include ignore_missing="yes">/etc/fonts/fonts.conf</include>
      <cachedir prefix="xdg">fontconfig</cachedir>
    </fontconfig>
  '';

  runtimeLibs = [
    vulkan-loader
    libGL
    wayland
    libxkbcommon
    libx11
    libxcursor
    libxi
    libxrandr
    libxcb
    fontconfig
  ];

  cargoToml = lib.importTOML ../console-rs/crates/uaconsole/Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "uaconsole";
  inherit (cargoToml.package) version;

  src = lib.fileset.toSource {
    root = ../console-rs;
    fileset = lib.fileset.unions [
      ../console-rs/Cargo.toml
      ../console-rs/Cargo.lock
      ../console-rs/crates
      ../console-rs/packaging
    ];
  };

  # No git dependencies, so the lock file alone pins every crate (no cargoHash to bump).
  cargoLock.lockFile = ../console-rs/Cargo.lock;

  # The workspace also holds helper binaries; ship only the app.
  cargoBuildFlags = [
    "--package"
    "uaconsole"
  ];
  # Unit tests for every crate. The build sandbox has no GPU and no UA resources:
  # tests that need either must skip themselves (see console-rs/README.md).
  cargoTestFlags = [ "--workspace" ];

  nativeBuildInputs = [ makeWrapper ];

  # rpath for the dlopen'd libraries; the fixup phase would otherwise shrink it away.
  dontPatchELF = true;

  postInstall = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/uaconsole
    wrapProgram $out/bin/uaconsole --set FONTCONFIG_FILE ${fontsConf}

    install -Dm644 packaging/uaconsole.desktop $out/share/applications/uaconsole.desktop
    install -Dm644 packaging/uaconsole.svg $out/share/icons/hicolor/scalable/apps/uaconsole.svg
  '';

  passthru = {
    inherit runtimeLibs fontsConf nimbus-sans;
  };

  meta = {
    description = "Native mixer console for Universal Audio Apollo interfaces on open-apollo";
    longDescription = ''
      A Rust rebuild of UA's UAD Console mixer that talks to the open-apollo
      mixer daemon (or UA's engine) on TCP 4710. UA's art is not included:
      it is loaded at run time from the user's own UAD Console install.
    '';
    homepage = "https://github.com/rolotrealanis98/open-apollo/tree/master/console-rs";
    license = lib.licenses.gpl2Only;
    mainProgram = "uaconsole";
    platforms = lib.platforms.linux;
  };
}
