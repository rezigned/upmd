{
  pkgs ? import <nixpkgs> {},
  lib ? pkgs.lib,
  rustPlatform,
  python3,
}: let
  manifest = lib.importTOML ./Cargo.toml;
in
  rustPlatform.buildRustPackage {
    pname = manifest.package.name;
    version = manifest.package.version;

    nativeBuildInputs = [
      # Needed for test:
      # runner::tests::unix::test_python_state_capture_roundtrip
      python3
    ];

    src = lib.cleanSource ./.;

    cargoLock.lockFile = ./Cargo.lock;
    cargoLock.outputHashes = {
      "vt100-0.16.2" = "sha256-18opt/6FxwTx1CfWUEUm3QajujJvddBdvFDkRpopTtE=";
    };

    checkFlags = [
      # FIXME: Skipped because NixOS is not guaranteed to have /bin/bash,
      # which these tests rely on
      #
      # These tests pass in NixOS if /bin/bash is available
      "--skip=runner::tests::unix::test_bin_attr_without_config"
      "--skip=runner::tests::unix::test_bin_attr_takes_precedence_over_config"

      # Failing (seems to fail due to non-interactive shell during nix build)
      "--skip=apps::cli::app::tests::test_write_card_contains_code_and_separator"
    ];

    meta = {
      description = "Run any code blocks in markdown from the terminal";
      mainProgram = manifest.package.name;
      license = lib.licenses.mit;
    };
  }
