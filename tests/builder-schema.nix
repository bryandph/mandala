{lib}: let
  f = import ./fixtures/builders.nix {inherit lib;};
  inherit (f) engine fails capacity;
  member = builder:
    engine.evalMember {
      name = "acme";
      inherit builder;
    };
  invalid = patch: fails (member (capacity // patch));
  direct = builder:
    (lib.evalModules {
      modules = [
        engine.schemas.member
        {
          options.assertions = lib.mkOption {
            default = [];
            type = lib.types.listOf lib.types.raw;
          };
          config.host = {
            name = "acme";
            inherit builder;
          };
        }
      ];
    }).config.host;
  good = member (capacity // {lanes = {};});
  lanes = engine.builders.effectiveLanes (member capacity).builder;
in
  assert (member null).builder == null;
  assert good.builder.maxJobs == 1 && good.builder.speedFactor == 1;
  assert good.builder.port == 22 && good.builder.protocol == "ssh-ng";
  assert !good.deployment.deployRs.enable && !good.deployment.ansible.enable;
  assert good.build == null;
  assert (member capacity).builder == (direct capacity).builder;
  assert (member (capacity
    // {
      trust = {
        kind = "ca";
        publicKey = f.pin.publicKey;
      };
    })).builder.trust.principal
  == null;
  assert (direct (capacity
    // {
      trust = {
        kind = "ca";
        publicKey = f.pin.publicKey;
      };
    })).builder.trust.principal
  == null;
  assert good.builder.systems == ["aarch64-linux" "x86_64-linux"];
  assert good.builder.supportedFeatures == ["big-parallel" "kvm"];
  assert builtins.attrNames (engine.builders.effectiveLanes good.builder) == ["default"];
  assert builtins.attrNames lanes == ["serial" "wide"];
  assert lanes.serial.supportedFeatures == [] && lanes.serial.mandatoryFeatures == [];
  assert lanes.serial.systems == ["x86_64-linux"];
  assert lanes.wide.maxJobs == 8 && lanes.wide.mandatoryFeatures == ["kvm"];
  assert lib.all invalid [
    {systems = [];}
    {systems = "x86_64-linux";}
    {maxJobs = 0;}
    {port = 0;}
    {port = 65536;}
    {unrecognized = true;}
    {maxJobs = 1.5;}
    {speedFactor = -1;}
    {mandatoryFeatures = ["missing"];}
    {lanes.bad.supportedFeatures = [];}
    {lanes.bad.systems = [];}
    {lanes."bad name" = {};}
    {protocol = "ssh";}
    {sshUser = "root\nProxyCommand bad";}
    {hostname = "host?query";}
    {
      trust = {
        kind = "pin";
        publicKey = f.pin.publicKey;
        principal = "not-for-pins";
      };
    }
    {
      trust = {
        kind = "pin";
        publicKey = "not-a-key";
      };
    }
  ];
  assert fails (direct (capacity // {maxJobs = 0;}));
  assert fails (direct (capacity // {port = 65536;}));
  assert fails (direct (capacity // {mandatoryFeatures = ["missing"];}));
  assert invalid {
    trust = {
      kind = "pin";
      publicKey = "ssh-ed25519 A";
    };
  };
  assert fails (direct (capacity // {lanes.invalid.supportedFeatures = [];})); true
