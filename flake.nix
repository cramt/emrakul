{
  description = "emrakul: a single-purpose Wayland compositor that turns a PC into a TV appliance";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {inherit system overlays;};

      rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

      src = craneLib.cleanCargoSource ./.;

      commonArgs = {
        inherit src;
        strictDeps = true;
        nativeBuildInputs = [pkgs.pkg-config];
        buildInputs = with pkgs; [
          libglvnd
          libinput
          libxkbcommon
          libgbm
          seatd
          systemd # libudev
          wayland
        ];
        # Smithay dlopen()s libEGL and libwayland at run time, so nothing links
        # them and the store path never makes it into the binary's RUNPATH.
        # Forcing the link puts them there. Same trick as nixpkgs' niri.
        RUSTFLAGS = toString (map (arg: "-C link-arg=" + arg) [
          "-Wl,--push-state,--no-as-needed"
          "-lEGL"
          "-lwayland-client"
          "-Wl,--pop-state"
        ]);
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      emrakul = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          meta.mainProgram = "emrakul";
        });
    in {
      packages.default = emrakul;

      checks = {
        inherit emrakul;
        clippy = craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
        fmt = craneLib.cargoFmt {inherit src;};
        test = craneLib.cargoTest (commonArgs // {inherit cargoArtifacts;});
      };

      devShells.default = craneLib.devShell {
        inputsFrom = [emrakul];
        packages = with pkgs; [rust-analyzer cargo-nextest];
      };
    })
    // {
      nixosModules.default = import ./nix/module.nix self;
    };
}
