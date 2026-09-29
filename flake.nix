{
  description = "Output-aware keybind router for niri/biri";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          nah = pkgs.rustPlatform.buildRustPackage {
            pname = "nah";
            inherit version;
            src = self;

            cargoLock.lockFile = ./Cargo.lock;

            # `cargo test` runs in the sandbox: 49 tests (36 unit, 8
            # daemon_client, 4 hardening, 1 selftest) over temp UNIX sockets.
            doCheck = true;

            meta = {
              description = "Output-aware keybind router for niri/biri";
              license = pkgs.lib.licenses.mit;
              mainProgram = "nah";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        in
        {
          default = nah;
          inherit nah;
        });

      overlays.default = final: prev: {
        nah = self.packages.${final.stdenv.hostPlatform.system}.default;
      };
    };
}
