# Render one validated controller projection; never read identity paths.
{lib}: profile: let
  safe = pattern: value:
    builtins.isString value && !builtins.hasContext value && builtins.match pattern value != null;
  alias = safe "[A-Za-z0-9][A-Za-z0-9._-]*";
  endpoint = safe "[A-Za-z0-9][A-Za-z0-9.:-]*";
  user = safe "[A-Za-z_][A-Za-z0-9_-]*";
  path = safe "/[A-Za-z0-9/._+%=-]+";
  token = safe "[A-Za-z0-9][A-Za-z0-9._+-]*";
  key = safe "(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(256|384|521)) [A-Za-z0-9+/]+={0,2}";
  port = n: builtins.isInt n && n >= 1 && n <= 65535;
  positive = n: builtins.isInt n && n > 0;
  list = predicate: xs: builtins.isList xs && lib.all predicate xs;
  sshValid = name: r:
    alias name
    && endpoint r.hostname
    && user r.sshUser
    && port r.port
    && path r.identityFile
    && endpoint r.hostKeyAlias
    && (r.jumpAlias == null || (alias r.jumpAlias && profile.ssh ? ${r.jumpAlias}))
    && r.strictHostKeyChecking == true
    && r.identitiesOnly == true
    && r.identityAgent == "none";
  trustValid = name: h: let
    r = profile.ssh.${name};
    match =
      if r.port == 22
      then r.hostKeyAlias
      else "[${r.hostKeyAlias}]:${toString r.port}";
  in
    profile.ssh ? ${name}
    && key h.publicKey
    && h.hostNames == [match]
    && builtins.isBool h.certAuthority
    && h.certAuthority == (r.trust.kind == "ca")
    && h.publicKey == r.trust.publicKey
    && (
      if h.certAuthority
      then r.hostKeyAlias == r.trust.principal
      else r.trust.kind == "pin"
    );
  machineValid = m:
    safe "[A-Za-z0-9][A-Za-z0-9._-]*(\\?remote-program=([A-Za-z0-9._~%-]|%[A-Fa-f0-9][A-Fa-f0-9])*)?" m.hostName
    && profile.ssh ? ${lib.head (lib.splitString "?" m.hostName)}
    && m.protocol == "ssh-ng"
    && user m.sshUser
    && path m.sshKey
    && list token m.systems
    && m.systems != []
    && positive m.maxJobs
    && positive m.speedFactor
    && list token m.supportedFeatures
    && list token m.mandatoryFeatures
    && (m.publicHostKey == null || safe "[A-Za-z0-9+/]+={0,2}" m.publicHostKey);
  valid =
    lib.all (name: sshValid name profile.ssh.${name}) (lib.attrNames profile.ssh)
    && lib.attrNames profile.ssh == lib.attrNames profile.knownHosts
    && lib.all (name: trustValid name profile.knownHosts.${name}) (lib.attrNames profile.knownHosts)
    && lib.all machineValid profile.buildMachines;
  # OpenSSH expands percent tokens even inside quotes; preserve literal paths.
  identity = value: lib.replaceStrings ["%"] ["%%"] value;
  sshConfig =
    lib.concatStrings (lib.mapAttrsToList (name: r:
      ''
        Host ${name}
          HostName ${r.hostname}
          User ${r.sshUser}
          Port ${toString r.port}
          IdentityFile ${identity r.identityFile}
          IdentitiesOnly yes
          IdentityAgent none
          StrictHostKeyChecking yes
          HostKeyAlias ${r.hostKeyAlias}
      ''
      + lib.optionalString (r.jumpAlias != null) "  ProxyJump ${r.jumpAlias}\n")
    profile.ssh)
    + lib.optionalString (profile.ssh != {}) "Host *\n";
  knownHosts = lib.mapAttrs' (name: h:
    lib.nameValuePair "mandala-${name}" {
      inherit (h) hostNames publicKey certAuthority;
    })
  profile.knownHosts;
  knownHostsText = lib.concatStrings (lib.mapAttrsToList (_: h:
    lib.optionalString h.certAuthority "@cert-authority "
    + lib.concatStringsSep "," h.hostNames
    + " "
    + h.publicKey
    + "\n")
  knownHosts);
  csv = xs:
    if xs == []
    then "-"
    else lib.concatStringsSep "," xs;
  # Match the standard NixOS/nix-darwin >=2.4 eight-field renderer, including
  # its append of mandatoryFeatures to supportedFeatures (no lane deduplication).
  machinesFile = lib.concatMapStrings (m:
    lib.concatStringsSep " " [
      "${m.protocol}://${m.sshUser}@${m.hostName}"
      (csv m.systems)
      m.sshKey
      (toString m.maxJobs)
      (toString m.speedFactor)
      (csv (m.supportedFeatures ++ m.mandatoryFeatures))
      (csv m.mandatoryFeatures)
      (
        if m.publicHostKey == null
        then "-"
        else m.publicHostKey
      )
    ]
    + "\n")
  profile.buildMachines;
in
  assert lib.assertMsg valid "mandala builders: unsafe renderer input or inconsistent strict SSH/trust model"; {inherit sshConfig knownHosts knownHostsText machinesFile;}
