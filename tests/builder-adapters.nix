{
  lib,
  nixpkgs,
}: let
  model = import ./fixtures/builder-adapter-model.nix {inherit lib;};
  exports = (import ../flake.nix).outputs {
    self = {};
    inherit nixpkgs;
  };
  extra = "ServerAliveInterval 37\n";
  evaluate = platform: settings: contributions: let
    module =
      if platform == "nixos"
      then exports.nixosModules.builders
      else exports.darwinModules.builders;
    modules = [module {mandala.builders = settings;} contributions];
  in
    if platform == "nixos"
    then
      (lib.nixosSystem {
        system = "x86_64-linux";
        inherit modules;
      }).config
    else (lib.evalModules {modules = [./fixtures/builder-darwin-options.nix] ++ modules;}).config;
  enabled = id: {
    enable = true;
    controller = id;
    byController = model;
  };
  normalize = c: {
    distributedBuilds = c.nix.distributedBuilds;
    buildersUseSubstitutes = c.nix.settings.builders-use-substitutes;
    buildMachines = c.nix.buildMachines;
    sshConfig = c.programs.ssh.extraConfig;
    knownHosts = c.programs.ssh.knownHosts;
    rendered = c.mandala.builders.rendered;
  };
  # Native knownHosts submodules have the same defaults on these fields.
  relevant = c:
    (normalize c)
    // {
      knownHosts = lib.mapAttrs (_: h: lib.getAttrs ["hostNames" "publicKey" "certAuthority" "extraHostNames" "publicKeyFile"] h) c.programs.ssh.knownHosts;
    };
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
  other = {
    hostName = "other.globex.example";
    systems = ["x86_64-linux"];
  };
  test = platform: let
    eval = evaluate platform;
    cases = lib.genAttrs (lib.attrNames model) (id: relevant (eval (enabled id) {programs.ssh.extraConfig = extra;}));
    merged = eval (enabled "wsl") {nix.buildMachines = [other];};
    exclusive = eval (enabled "wsl" // {exclusiveBuildMachines = true;}) {nix.buildMachines = [other];};
    disabled = eval {} {};
  in
    assert builtins.toJSON cases == lib.removeSuffix "\n" (builtins.readFile (./fixtures/builder-adapter-golden + "/${platform}.json"));
    assert lib.all (id: cases.${id}.sshConfig == cases.${id}.rendered.sshConfig + "\n" + extra) (lib.attrNames model);
    assert lib.hasInfix "Host *\n\nServerAliveInterval 37\n" cases.wsl.sshConfig;
    assert builtins.length merged.nix.buildMachines == 4;
    assert lib.any (m: m.hostName == other.hostName) merged.nix.buildMachines;
    assert builtins.length exclusive.nix.buildMachines == 3;
    assert !(lib.any (m: m.hostName == other.hostName) exclusive.nix.buildMachines);
    assert !disabled.nix.distributedBuilds && disabled.nix.buildMachines == [] && disabled.programs.ssh.extraConfig == "" && disabled.programs.ssh.knownHosts == {};
    assert fails (normalize (eval (enabled "missing") {}));
    assert fails (normalize (eval {
      enable = true;
      byController = model;
    } {}));
    # Ordinary true settings conflict with explicit false (not mkDefault).
    assert fails (normalize (eval (enabled "wsl") {nix.distributedBuilds = false;}));
    # NixOS's actual generated machines file must match inspection exactly.
    assert platform != "nixos" || lib.all (id: id == "empty" || (eval (enabled id) {}).environment.etc."nix/machines".text == cases.${id}.rendered.machinesFile) (lib.attrNames model); true;
in
  assert test "nixos";
  assert test "darwin"; true
