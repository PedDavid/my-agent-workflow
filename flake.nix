{
  description = "drove: agent herding built into Hyprland + kitty";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = nixpkgs.lib;

      drove = pkgs.rustPlatform.buildRustPackage {
        pname = "drove";
        version = (lib.importTOML ./Cargo.toml).package.version;
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            (lib.fileset.maybeMissing ./tests)
            (lib.fileset.maybeMissing ./contrib)
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;
        # Unit/integration tests run via `cargo test` in the dev shell; the VM
        # checks below are the end-to-end gate.
        doCheck = false;
      };

      fakeAgents = import ./nix/fake-agents.nix { inherit pkgs; };
    in
    {
      packages.${system} = {
        inherit drove;
        default = drove;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer jq ];
      };

      checks.${system} = {
        # Real Hyprland (Lua config) + kitty in a QEMU VM. Validates the IPC
        # assumptions drove relies on; independent of drove itself.
        vm-hyprland = import ./nix/vm-hyprland.nix { inherit pkgs; };

        # End-to-end acceptance test for drove against real Hyprland + kitty,
        # with scripted fake claude/codex/kiro-cli agents that fire real hooks.
        vm-drove = import ./nix/vm-drove.nix { inherit pkgs drove fakeAgents; };
      };
    };
}
