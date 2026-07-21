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
  presentScalars = attrs:
    lib.filterAttrs
    (_: value: value != null)
    (removeAttrs attrs ["sshOpts"]);
  sshOptsOf = attrs:
    if (attrs.sshOpts or null) == null
    then []
    else attrs.sshOpts;
  groupScalars =
    lib.foldl'
    (acc: name: lib.recursiveUpdate acc (presentScalars groupSettings.${name}))
    {}
    applicableGroups;
  scalars =
    lib.recursiveUpdate
    (lib.recursiveUpdate (presentScalars fleet) groupScalars)
    (presentScalars member);
  effectiveScalars =
    {
      autoRollback = true;
      fastConnection = true;
    }
    // scalars;
  sshOpts =
    (sshOptsOf member)
    ++ lib.concatMap (name: sshOptsOf groupSettings.${name}) applicableGroups
    ++ (sshOptsOf fleet);
in
  assert lib.assertMsg (unknownGroups == [])
  "mandala deploy settings: unknown groupSettings keys: ${lib.concatStringsSep ", " unknownGroups}";
    effectiveScalars // lib.optionalAttrs (sshOpts != []) {inherit sshOpts;}
