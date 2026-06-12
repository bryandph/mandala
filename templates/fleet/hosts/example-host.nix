# One NixOS member. `host.*` IS the fleet inventory entry — the same
# evaluation that builds the system authors the contract data every
# projection reads. No registry to update, nothing to keep in sync.
{
  host = {
    name = "example-host";
    domain = "fleet.example";
    category = "server";
    role = "web";
    tags = ["example"];
    # Opt into the management surfaces this member should appear on:
    deployment.ansible.enable = true;
    # deployment.deployRs.enable = true; # with the deploy flakeModule
    # deployment.sops.recipient = "age1..."; # with the sops flakeModule
  };

  # Minimal bootable stand-in so the configuration evaluates; replace
  # with your real hardware configuration.
  boot.loader.grub.device = "/dev/sda";
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
  };
  system.stateVersion = "26.05";
}
