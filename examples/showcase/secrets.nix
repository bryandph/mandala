# Fictional secret-grade declarations, one per reader form. Validated by
# lib.evalSecrets against the showcase fleet (the sops flakeModule does
# this) — a reader without a recipient would fail `nix flake check` here.
{
  web-tls = {
    path = "secrets/web.yaml";
    readers.members = ["web"];
    custody.keySource = "external";
    custody.note = "issued by the fictional CA; rotate yearly";
  };
  cluster = {
    path = "secrets/cluster.yaml";
    readers.groups = ["cache"]; # role group, resolves via the taxonomy
  };
  fleet = {
    path = "secrets/fleet.yaml";
    readers.all = true; # every member with a sops identity
  };
  host-keys = {
    path = "secrets/host-keys.yaml";
    readers.adminOnly = true; # seals to the operator anchor alone
    custody.keySource = "generated";
  };
}
