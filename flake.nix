{
  description = "flume: a minimal, fast build system for Lean 4 (Rust driver, Lean worker, stock Lean as a library)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            # The driver (crates/)
            cargo
            rustc
            rustfmt
            clippy
            # Installs the stock Lean toolchain pinned in `lean-toolchain`, which the
            # worker (worker/) links and every build runs.
            elan
            # Benchmark harness (bench/) and tooling
            python3
            git
            jq
            gh
            gnumake
          ];
          # nixpkgs' elan wraps the toolchain's own `lake`/`leanc` to export LEAN_CC;
          # executables linked by other tools need the same on NixOS.
          shellHook = ''
            if [ -z "''${LEAN_CC:-}" ] && command -v lean >/dev/null 2>&1; then
              p=$(lean --print-prefix 2>/dev/null) && [ -x "$p/bin/cc" ] && export LEAN_CC="$p/bin/cc"
            fi
          '';
        };
      });
    };
}
