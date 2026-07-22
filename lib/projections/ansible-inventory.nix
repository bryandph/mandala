# Ansible inventory projection: validated members in, inventory attrset out
# (the shape `ansible-inventory --list` expects under `all`). Pure data —
# the consumer decides how to serve it (typically `nix eval --json` behind
# a dynamic inventory script; nothing is cached or committed).
#
# Membership: every member with deployment.ansible.enable — the schema's
# class-agnostic opt-in. Group children come from ansibleGroupsFor, the one
# taxonomy behind every fan-out surface (deploy batches, sops recipient
# groups), already in the ansible-safe spelling.
#
# NixOS conventions are emitted by default and overridable:
#   - ansible_python_interpreter pins NixOS members to their system-profile
#     python3 (NixOS has no /usr/bin/python3 for ansible's interpreter
#     auto-discovery); pythonInterpreter = null omits the var. A member is
#     NixOS when platform == "nixos" OR it has a consumer-built closure
#     (host.build != null, the schema's NixOS-member marker) — the platform
#     field carries the hosting VENUE for cloud members ("hetzner", "gcp"),
#     so it alone can't identify the OS family.
#   - a synthetic guard group (default `deploy_rs`) of the members
#     deploy-rs can activate, so a fan-out playbook's --limit can never
#     reach a facts-only or ansible-only member; guardGroup = null omits it.
#
# Consumer-specific vars enter via extraHostvars (name -> attrset), merged
# AFTER the engine defaults so a hook can override convention values; the
# member's own deployment.ansible.vars merge last (most specific wins).
{
  lib,
  ansibleGroupsFor,
}: {
  # name -> validated member (a NixOS host's `config.host`, or an
  # evalMember result for non-NixOS members).
  hosts,
  # Consumer hook: name -> attrset of extra hostvars for that member.
  extraHostvars ? (_name: {}),
  # name -> settings already flattened by the fleet module's single merge.
  # Optional for direct library consumers and unchanged-inventory parity.
  deploySettings ? {},
  # ansible_python_interpreter value for platform == "nixos" members.
  pythonInterpreter ? "/run/current-system/sw/bin/python3",
  # Name of the synthetic deploy-rs guard group.
  guardGroup ? "deploy_rs",
}: let
  members = lib.filterAttrs (_: host: host.deployment.ansible.enable) hosts;

  names = lib.attrNames members;

  isNixos = host: host.platform == "nixos" || host.build != null;

  hostvarsFor = name: host: let
    d = host.deployment;
    deploy = deploySettings.${name} or {};
    hostname = deploy.hostname or d.ssh.host;
    sshUser = deploy.sshUser or d.ssh.user;
    sshPort = deploy.sshPort or d.ssh.port;
    identityFile = deploy.identityFile or null;
    targetUser = deploy.user or null;
    sudo = deploy.sudo or null;
    needsBecome = targetUser != null && targetUser != sshUser;
    becomeMethod =
      if !needsBecome
      then null
      else if sudo == null || sudo == "sudo -u"
      then "sudo"
      else if sudo == "doas -u"
      then "doas"
      else null;
  in
    {
      ansible_host = hostname;
      ansible_user = sshUser;
    }
    // lib.optionalAttrs (isNixos host && pythonInterpreter != null) {
      ansible_python_interpreter = pythonInterpreter;
    }
    // lib.optionalAttrs (sshPort != 22) {ansible_port = sshPort;}
    // lib.optionalAttrs (identityFile != null) {
      ansible_ssh_private_key_file = identityFile;
      ansible_ssh_common_args = "-o IdentitiesOnly=yes -o IdentityAgent=none";
    }
    // lib.optionalAttrs (targetUser != null) {
      mandala_deploy_target_user = targetUser;
    }
    // lib.optionalAttrs (becomeMethod != null) {
      ansible_become = true;
      ansible_become_user = targetUser;
      ansible_become_method = becomeMethod;
    }
    // lib.optionalAttrs (sudo != null) {
      mandala_deploy_sudo = sudo;
    }
    // extraHostvars name
    // d.ansible.vars;

  allGroups = lib.unique (
    lib.concatMap (name: ansibleGroupsFor members.${name}) names
  );

  hostsInGroup = group:
    lib.filter
    (name: lib.elem group (ansibleGroupsFor members.${name}))
    names;

  hostsToNullValues = hosts':
    lib.listToAttrs (map (h: {
        name = h;
        value = null;
      })
      hosts');

  guardHosts =
    lib.filter
    (name: members.${name}.deployment.deployRs.enable)
    names;
in {
  all = {
    hosts = lib.mapAttrs hostvarsFor members;
    children =
      lib.listToAttrs
      (map (g: {
          name = g;
          value.hosts = hostsToNullValues (hostsInGroup g);
        })
        allGroups)
      // lib.optionalAttrs (guardGroup != null) {
        ${guardGroup}.hosts = hostsToNullValues guardHosts;
      };
  };
}
