# Minimal option-only contract checked read-only against nix-darwin revision
# ff0e21a414d4547183396da1b44a66f2577b65e9, modules/nix/default.nix and
# modules/programs/ssh.nix. No Darwin activation logic or flake input.
{lib, ...}: let
  inherit (lib) mkOption types;
  opt = type: default: mkOption {inherit type default;};
  atom = types.nullOr (types.oneOf [types.bool types.int types.float types.str types.path types.package]);
in {
  options = {
    nix.distributedBuilds = opt types.bool false;
    nix.settings = opt (types.submodule {
      freeformType = types.attrsOf (types.either atom (types.listOf atom));
    }) {};
    nix.buildMachines = opt (types.listOf (types.submodule {
      options = {
        hostName = mkOption {type = types.str;};
        protocol = opt (types.enum [null "ssh" "ssh-ng"]) "ssh";
        system = opt (types.nullOr types.str) null;
        systems = opt (types.listOf types.str) [];
        sshUser = opt (types.nullOr types.str) null;
        sshKey = opt (types.nullOr types.str) null;
        maxJobs = opt types.int 1;
        speedFactor = opt types.int 1;
        supportedFeatures = opt (types.listOf types.str) [];
        mandatoryFeatures = opt (types.listOf types.str) [];
        publicHostKey = opt (types.nullOr types.str) null;
      };
    })) [];
    programs.ssh.extraConfig = opt types.lines "";
    programs.ssh.knownHosts = opt (types.attrsOf (types.submodule ({
      name,
      config,
      ...
    }: {
      options = {
        certAuthority = opt types.bool false;
        hostNames = opt (types.listOf types.str) ([name] ++ config.extraHostNames);
        extraHostNames = opt (types.listOf types.str) [];
        publicKey = opt (types.nullOr types.str) null;
        publicKeyFile = opt (types.nullOr types.path) null;
      };
    }))) {};
  };
}
