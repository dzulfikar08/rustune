{
  description = "rustune: a terminal music player with online search, radio, podcasts and Winamp skins";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.rustune;
        rustune = pkgs.rustPlatform.buildRustPackage {
          pname = "rustune";
          inherit (cargoToml.package) version;

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./README.md
              ./LICENSE
              ./src
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          # cpal's ALSA backend on Linux; macOS frameworks come from the
          # default SDK.
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.pkg-config ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.alsa-lib ];

          # The test suite runs in CI; a fat-LTO release build is enough here.
          doCheck = false;

          meta = {
            description = cargoToml.package.description;
            homepage = cargoToml.package.homepage;
            license = pkgs.lib.licenses.mit;
            mainProgram = "rustune";
            platforms = systems;
          };
        };
      });
    };
}
