{
  description = "nzbg – efficient Usenet downloader";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    {
      nixosModules.default = import ./nix/module.nix { inherit self; };
    }
    //
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        isLinux = pkgs.stdenv.hostPlatform.isLinux;

        nzbg = pkgs.rustPlatform.buildRustPackage {
          pname = "nzbg";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        runtimeDeps = with pkgs; [
          unrar-free
          p7zip
          python3
        ];

        containerConfig = {
          name = "nzbg";
          tag = "latest";
          contents = [
            nzbg
            pkgs.cacert
            pkgs.busybox
          ] ++ runtimeDeps;
          config = {
            Entrypoint = [ "/bin/nzbg" ];
            Cmd = [ "--foreground" "--config" "/config/nzbg.conf" ];
            ExposedPorts = {
              "6789/tcp" = { };
            };
            Volumes = {
              "/config" = { };
              "/downloads" = { };
            };
            Env = [
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
            ];
          };
        };
      in
      {
        packages =
          { default = nzbg; }
          // pkgs.lib.optionalAttrs isLinux {
            docker = pkgs.dockerTools.buildLayeredImage containerConfig;
            docker-stream = pkgs.dockerTools.streamLayeredImage containerConfig;
          };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustc
            cargo
            cargo-watch
            clippy
            rustfmt
            rust-analyzer
          ] ++ runtimeDeps;

          RUST_BACKTRACE = 1;
        };
      }
    );
}
