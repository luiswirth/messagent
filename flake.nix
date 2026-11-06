{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    # The relay, for the bridge's test.
    polylogue.url = "github:luiswirth/polylogue";
    polylogue.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    treefmt-nix,
    polylogue,
    ...
  }: let
    forEachSystem = f:
      nixpkgs.lib.genAttrs ["aarch64-darwin" "x86_64-linux"]
      (system:
        f (import nixpkgs {
          inherit system;
          overlays = [(import rust-overlay)];
        }));

    # One environment, entered by the shell and by the checks alike.
    tools = pkgs:
      with pkgs; [
        (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
        bacon
        cargo-edit
        just
        pkg-config
        polylogue.packages.${pkgs.stdenv.hostPlatform.system}.default
        python3
      ];
  in {
    # Every language in the tree, behind one command. The global pre-commit
    # hook checks a repository by running this, so a language left out here is
    # a language nothing checks.
    formatter = forEachSystem (pkgs:
      treefmt-nix.lib.mkWrapper pkgs {
        projectRootFile = "flake.nix";
        programs.rustfmt.enable = true;
        programs.alejandra.enable = true;
        programs.just.enable = true;
      });

    # Both binaries: `messagent`, and `messagent-claude`, which wakes Claude Code.
    packages = forEachSystem (pkgs: {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "messagent";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [pkgs.pkg-config];
        buildInputs = [pkgs.libopus];
      };
    });

    devShells = forEachSystem (pkgs: {
      default = pkgs.mkShell {packages = tools pkgs;};
    });
  };
}
