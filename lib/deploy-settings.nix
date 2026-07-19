# Pure three-tier deploy-settings merge. Member scalars override the sorted
# group fold, which overrides fleet defaults. sshOpts preserve deploy-rs's
# innermost-first append semantics: member, groups, fleet.
{lib}: {
  knownGroups,
  fleet ? {},
  groupSettings ? {},
  memberGroups ? [],
  member ? {},
}: let
  unknownGroups = lib.filter (name: !(lib.elem name knownGroups)) (lib.attrNames groupSettings);
  sortedMemberGroups = lib.sort (a: b: a < b) (lib.unique memberGroups);
  applicableGroups = lib.filter (name: groupSettings ? ${name}) sortedMemberGroups;
  withoutSshOpts = attrs: builtins.removeAttrs attrs ["sshOpts"];
  groupScalars =
    lib.foldl'
    (acc: name: lib.recursiveUpdate acc (withoutSshOpts groupSettings.${name}))
    {}
    applicableGroups;
  scalars =
    lib.recursiveUpdate
    (lib.recursiveUpdate (withoutSshOpts fleet) groupScalars)
    (withoutSshOpts member);
  sshOpts =
    (member.sshOpts or [])
    ++ lib.concatMap (name: groupSettings.${name}.sshOpts or []) applicableGroups
    ++ (fleet.sshOpts or []);
in
  assert lib.assertMsg (unknownGroups == [])
  "mandala deploy settings: unknown groupSettings keys: ${lib.concatStringsSep ", " unknownGroups}";
    scalars // lib.optionalAttrs (sshOpts != []) {inherit sshOpts;}
