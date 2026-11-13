# Pure capacity normalization and strict controller projection. No key-file IO.
{lib}: let
  inherit (lib) types mkOption;
  require = context: condition: value:
    assert lib.assertMsg condition "mandala builders: ${context}"; value;
  label = types.strMatching "[A-Za-z0-9][A-Za-z0-9_-]*";
  token = types.strMatching "[A-Za-z0-9][A-Za-z0-9._+-]*";
  absolute = types.addCheck (types.strMatching "/[A-Za-z0-9/._+%=-]+") (s: !builtins.hasContext s);
  endpoint = types.strMatching "[A-Za-z0-9][A-Za-z0-9.:-]*";
  key =
    types.addCheck
    (types.strMatching "(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(256|384|521)) [A-Za-z0-9+/]+={0,2}")
    (text: let
      blob = lib.last (lib.splitString " " text);
      length = builtins.stringLength blob;
    in
      length >= 16 && length - (builtins.div length 4) * 4 == 0);
  option = type: default: mkOption {inherit type default;};
  nullable = type: option (types.nullOr type) null;
  trustType = types.submodule {
    options = {
      kind = mkOption {type = types.enum ["pin" "ca"];};
      publicKey = mkOption {type = key;};
      principal = nullable endpoint;
    };
  };
  connectionOptions = {
    hostname = nullable endpoint;
    sshUser = nullable (types.strMatching "[A-Za-z_][A-Za-z0-9_-]*");
    port = nullable (types.ints.between 1 65535);
    protocol = nullable (types.enum ["ssh-ng"]);
    remoteProgram = nullable absolute;
    trust = nullable trustType;
  };
  schedulingOptions = inherited: let
    field = type: default:
      if inherited
      then nullable type
      else option type default;
  in {
    systems =
      if inherited
      then nullable (types.listOf token)
      else mkOption {type = types.listOf token;};
    supportedFeatures = field (types.listOf token) [];
    mandatoryFeatures = field (types.listOf token) [];
    maxJobs = field types.ints.positive 1;
    speedFactor = field types.ints.positive 1;
  };
  normalizeSchedule = context: s: let
    canonical = xs: lib.sort builtins.lessThan (lib.unique xs);
  in
    require "${context}: systems must be nonempty and mandatoryFeatures must be supported"
    (s.systems != [] && lib.all (x: lib.elem x s.supportedFeatures) s.mandatoryFeatures)
    (s
      // {
        systems = canonical s.systems;
        supportedFeatures = canonical s.supportedFeatures;
        mandatoryFeatures = canonical s.mandatoryFeatures;
      });
  scheduleFields = ["systems" "supportedFeatures" "mandatoryFeatures" "maxJobs" "speedFactor"];
  schedule = s: lib.getAttrs scheduleFields s;
  validTrust = t:
    t
    != null
    && (
      if t.kind == "ca"
      then t.principal != null
      else t.principal == null
    );
  # A CA principal is optional until the effective connection is resolved.
  validateTrust = context: t: require "${context}: pin requires only publicKey" (t == null || t.kind == "ca" || t.principal == null) t;
  normalizeFacet = f: let
    base = normalizeSchedule "host.builder" (schedule f);
    lanes =
      if f.lanes == {}
      then {default = {};}
      else f.lanes;
    resolved = lib.mapAttrs (name: lane:
      normalizeSchedule "host.builder.lanes.${name}" (base // lib.filterAttrs (_: v: v != null) lane))
    lanes;
  in
    builtins.deepSeq resolved (f
      // base
      // {
        trust = validateTrust "host.builder.trust" f.trust;
        lanes = lib.mapAttrs (_: lane:
          lib.mapAttrs (field: value:
            if lib.elem field ["systems" "supportedFeatures" "mandatoryFeatures"] && value != null
            then lib.sort builtins.lessThan (lib.unique value)
            else value)
          lane)
        f.lanes;
      });
  effectiveLanes = f:
    if f.lanes == {}
    then {default = normalizeSchedule "default lane" (schedule f);}
    else lib.mapAttrs (name: lane: normalizeSchedule "lane ${name}" (schedule f // lib.filterAttrs (_: v: v != null) lane)) f.lanes;
  # JSON tuple identity avoids delimiter ambiguity. A bounded digest keeps
  # aliases below 63 characters; projection checks for collisions before use.
  alias = kind: controller: member: lane:
    "mb-${kind}-" + builtins.substring 0 56 (builtins.hashString "sha256" (builtins.toJSON [controller member lane]));
  # Pins are ASCII by schema. Nix publicHostKey encodes the WHOLE key text.
  base64 = value: let
    alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    chars = map lib.strings.charToInt (lib.stringToCharacters value);
    at = n:
      if n < builtins.length chars
      then builtins.elemAt chars n
      else 0;
    digit = n: builtins.substring n 1 alphabet;
    encode = n:
      if n >= builtins.length chars
      then ""
      else let
        a = at n;
        b = at (n + 1);
        c = at (n + 2);
        div = builtins.div;
        mod = x: y: x - (div x y) * y;
      in
        digit (div a 4)
        + digit ((mod a 4) * 16 + div b 16)
        + (
          if n + 1 < builtins.length chars
          then digit ((mod b 16) * 4 + div c 64)
          else "="
        )
        + (
          if n + 2 < builtins.length chars
          then digit (mod c 64)
          else "="
        )
        + encode (n + 3);
  in
    encode 0;
in rec {
  inherit effectiveLanes base64;
  facetType = types.submodule {
    options =
      schedulingOptions false
      // connectionOptions
      // {
        port = option (types.ints.between 1 65535) 22;
        protocol = option (types.enum ["ssh-ng"]) "ssh-ng";
        lanes = option (types.attrsOf (types.submodule {options = schedulingOptions true;})) {};
      };
  };
  facetOption = mkOption {
    type = types.nullOr facetType;
    default = null;
    apply = f:
      if f == null
      then null
      else require "host.builder: unsafe lane name" (lib.all (n: label.check n) (lib.attrNames f.lanes)) (normalizeFacet f);
    description = "Optional offered build capacity, independent of closure builds and deployment identities. Lanes replace the default advertisement; limits are not reservations.";
  };
  controllerType = types.submodule {
    options = {
      selfMember = mkOption {type = types.str;};
      allowedBuilders = mkOption {
        type = types.listOf types.str;
        default = throw "mandala builders: controller allowedBuilders must be explicitly declared";
      };
      lanes = option (types.attrsOf (types.listOf label)) {};
      routes = option (types.attrsOf (types.submodule {
        options =
          connectionOptions
          // {
            identityFile = nullable absolute;
            jumpMember = nullable types.str;
          };
      })) {};
    };
  };
  project = {
    members,
    controllers,
  }: let
    evaluatedControllers =
      (lib.evalModules {
        modules = [
          {
            options.controllers = mkOption {type = types.attrsOf controllerType;};
            config.controllers = controllers;
          }
        ];
      }).config.controllers;
    projectController = id: p: let
      context = "controller ${id}";
      names = lib.attrNames members;
      allowed = p.allowedBuilders;
      routes = p.routes;
      routeNames = lib.attrNames routes;
      selectedLanes = member: let
        available = effectiveLanes members.${member}.builder;
        selected = p.lanes.${member} or (lib.attrNames available);
      in
        require "${context}: duplicate or unknown lane for ${member}"
        (builtins.length selected == builtins.length (lib.unique selected) && lib.all (n: available ? ${n}) selected)
        (lib.sort builtins.lessThan selected);
      referencesValid =
        lib.all (n: lib.elem n names) ([p.selfMember] ++ allowed ++ routeNames)
        && builtins.length allowed == builtins.length (lib.unique allowed)
        && lib.all (n: (members.${n}.builder or null) != null && (members.${n}.kind or "standalone") != "vm") allowed
        && lib.all (n: (members.${n}.kind or "standalone") != "vm") routeNames
        && lib.all (n: lib.elem n allowed) (lib.attrNames p.lanes);
      visit = seen: member:
        require "${context} route ${member}: cyclic jump graph" (!(lib.elem member seen))
        (let
          jump = routes.${member}.jumpMember;
        in
          if jump == null
          then true
          else
            require "${context} route ${member}: unknown jump ${jump}"
            (routes ? ${jump}) (visit (seen ++ [member]) jump));
      resolve = member: let
        host = members.${member};
        defaults =
          if (host.builder or null) == null
          then {}
          else lib.getAttrs (lib.attrNames connectionOptions) host.builder;
        authored = lib.filterAttrs (_: v: v != null) routes.${member};
        connection =
          {
            hostname = host.fqdn or null;
            sshUser = null;
            port = 22;
            protocol = "ssh-ng";
            remoteProgram = null;
            trust = null;
            identityFile = null;
            jumpMember = null;
          }
          // lib.filterAttrs (_: v: v != null) defaults // authored;
        # Resolve against the dialled endpoint, never the member name, jump
        # hostname, generated SSH alias or host:port spelling.
        r =
          connection
          // {
            trust =
              if connection.trust != null && connection.trust.kind == "ca" && connection.trust.principal == null
              then connection.trust // {principal = connection.hostname;}
              else connection.trust;
          };
      in
        require "${context} route ${member}: endpoint, login, key and strict trust required"
        (r.hostname != null && endpoint.check r.hostname && r.sshUser != null && r.identityFile != null && validTrust r.trust)
        r;
      normalizedRoutes = lib.genAttrs routeNames resolve;
      selected =
        lib.concatMap (member: map (lane: {inherit member lane;}) (selectedLanes member))
        (lib.sort builtins.lessThan allowed);
      output = lib.filter (entry: entry.member != p.selfMember) selected;
      targets = map (entry: entry // {alias = alias "t" id entry.member entry.lane;}) output;
      # Jump routes get their own alias; targets retain lane-specific identity.
      sshRecord = member: lane: let
        r = normalizedRoutes.${member};
      in
        r
        // {
          inherit member lane;
          jumpAlias =
            if r.jumpMember == null
            then null
            else alias "j" id r.jumpMember "";
          # OpenSSH verifies certificate principals against HostKeyAlias.
          hostKeyAlias =
            if r.trust.kind == "ca"
            then r.trust.principal
            else alias "h" id member "";
          strictHostKeyChecking = true;
          identitiesOnly = true;
          identityAgent = "none";
        };
      jumpChain = member: let
        jump = routes.${member}.jumpMember;
      in
        if jump == null
        then []
        else [jump] ++ jumpChain jump;
      jumps = lib.sort builtins.lessThan (lib.unique (lib.concatMap (entry: jumpChain entry.member) output));
      ssh = builtins.listToAttrs (map (entry: {
          name = entry.alias;
          value = sshRecord entry.member entry.lane;
        })
        targets
        ++ map (member: {
          name = alias "j" id member "";
          value = sshRecord member null;
        })
        jumps);
      knownHosts =
        lib.mapAttrs (_: r: {
          inherit (r) member hostname port hostKeyAlias;
          inherit (r.trust) kind publicKey principal;
          matchName =
            if r.trust.kind == "ca"
            then r.trust.principal
            else r.hostKeyAlias;
          hostNames = [
            (
              if r.port == 22
              then r.hostKeyAlias
              else "[${r.hostKeyAlias}]:${toString r.port}"
            )
          ];
          certAuthority = r.trust.kind == "ca";
        })
        ssh;
      buildMachines = map (entry: let
        r = normalizedRoutes.${entry.member};
        s = (effectiveLanes members.${entry.member}.builder).${entry.lane};
      in
        s
        // {
          hostName = entry.alias + lib.optionalString (r.remoteProgram != null) "?remote-program=${lib.strings.escapeURL r.remoteProgram}";
          inherit (r) protocol sshUser;
          sshKey = r.identityFile;
          publicHostKey =
            if r.trust.kind == "pin"
            then base64 r.trust.publicKey
            else null;
        })
      targets;
      aliases = lib.attrNames ssh;
    in
      require "${context}: unsafe profile name" (label.check id)
      (require "${context}: invalid member, allowlist or lane references" referencesValid
        (builtins.deepSeq (map selectedLanes allowed)
          (require "${context}: selected builder requires a route" (lib.all (e: routes ? ${e.member}) selected)
            (builtins.deepSeq (map (visit []) routeNames)
              (builtins.deepSeq normalizedRoutes
                (require "${context}: alias collision" (builtins.length aliases == builtins.length targets + builtins.length jumps)
                  {inherit buildMachines ssh knownHosts;}))))));
  in
    lib.mapAttrs projectController evaluatedControllers;
}
