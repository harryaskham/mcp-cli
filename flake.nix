{
  description = "mcp-cli — reusable JSON envelope and MCP stdio helpers for CLI projects";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      # `nix develop` (default shell) provides the Rust toolchain so CI on the
      # azure-ephemeral runners — which ship Nix but no bare toolchain — can run
      # `nix develop --command cargo ...` without a custom runner image.
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
          ];
        };
      });
    };
}
