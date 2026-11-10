{lib}: let
  engine = import ../lib {inherit lib;};
  model = import ./fixtures/builder-adapter-model.nix {inherit lib;};
  rendered = lib.mapAttrs (_: engine.renderBuilders) model;
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
  name = lib.head (lib.attrNames model.wsl.ssh);
  alter = changes: model.wsl // {ssh = model.wsl.ssh // {${name} = model.wsl.ssh.${name} // changes;};};
  badMachine = field: value: model.wsl // {buildMachines = [(lib.head model.wsl.buildMachines // {${field} = value;})];};
in
  assert lib.all (id: builtins.toJSON rendered.${id} == lib.removeSuffix "\n" (builtins.readFile (./fixtures/builder-adapter-golden + "/${id}.json"))) (lib.attrNames model);
  assert rendered.empty
  == {
    sshConfig = "";
    knownHosts = {};
    knownHostsText = "";
    machinesFile = "";
  };
  assert lib.all (changes: fails (engine.renderBuilders (alter changes))) [
    {hostname = "host\nHost *\n  StrictHostKeyChecking no";}
    {identityFile = "/key\nProxyCommand evil";}
    {sshUser = "builder other";}
    {jumpAlias = "missing";}
    {hostKeyAlias = "*.example";}
    {strictHostKeyChecking = false;}
    {identitiesOnly = false;}
    {identityAgent = "SSH_AUTH_SOCK";}
  ];
  assert lib.all (p: fails (engine.renderBuilders p)) [
    (badMachine "hostName" "host?remote-program=x\nnext")
    (badMachine "systems" ["x86_64-linux,evil"])
    (badMachine "sshKey" "/key with spaces")
    (badMachine "publicHostKey" "key\nnext")
    (model.wsl // {knownHosts = lib.mapAttrs (_: h: h // {publicKey = "ssh-ed25519 AAAA\n@cert-authority * evil";}) model.wsl.knownHosts;})
  ];
  assert lib.hasInfix "IdentityFile /keys/percent%%key\n" (engine.renderBuilders (alter {identityFile = "/keys/percent%key";})).sshConfig; true
