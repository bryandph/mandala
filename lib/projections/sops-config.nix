# sops config projection: operator anchor + member recipients + an ordered
# rule list in, `.sops.yaml` content (attrset) out. Pure data — the
# consumer renders it (pkgs.formats.yaml) and decides delivery (devenv
# symlink, committed file, …).
#
# Every creation rule carries the operator anchor in its pgp group so the
# operator can always decrypt. adminOnly rules carry NO age key at all —
# not `age = []` — so sops seals to the anchor alone and the generated
# rule reads like a hand-authored admin-only rule would.
#
# Determinism: keys and per-rule age recipients are sorted by MEMBER NAME
# (unique'd first), so regeneration is stable regardless of attrset
# iteration order. Rule order is the consumer's: sops uses the first
# matching rule, so more specific paths must precede broader ones — the
# engine must not re-sort them.
#
# A rule reader without a recipient is an eval failure naming both, never
# a silent omission. (Secret-grade declarations with richer cross-field
# asserts replace raw rule lists in schema/secrets.nix — this projection
# stays the rendering layer underneath.)
{lib}: {
  # Operator PGP fingerprint, present in every rule's pgp group.
  operatorAnchor,
  # name -> public age recipient, one per member with a sops identity.
  recipients,
  # Ordered list of { path; readers ? [member names]; adminOnly ? false }.
  rules,
}: let
  sortedNames = lib.sort (a: b: a < b) (lib.attrNames recipients);

  recipientOf = path: name:
    recipients.${name}
    or (throw "mandala sops-config: rule for ${path} names reader '${name}', which has no recipient");

  recipientsFor = path: readers:
    map (recipientOf path) (lib.sort (a: b: a < b) (lib.unique readers));

  mkRule = rule: {
    path_regex = "${lib.escapeRegex rule.path}$";
    key_groups = [
      (
        {pgp = [operatorAnchor];}
        // lib.optionalAttrs (!(rule.adminOnly or false)) {
          age = recipientsFor rule.path (rule.readers or []);
        }
      )
    ];
  };
in {
  keys = [operatorAnchor] ++ map (name: recipients.${name}) sortedNames;
  creation_rules = map mkRule rules;
}
