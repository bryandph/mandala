# NixOS member: host.* IS the inventory entry. The minimal boot stanza
# below only exists so the configuration evaluates — a real host carries
# real hardware config here.
{
  host = {
    name = "web";
    domain = "fleet.example";
    category = "server";
    role = "web";
    tags = ["showcase"];
    networks = [
      {
        vlan = "lan";
        address = "10.10.0.10";
        interface = "eth0";
        roles = ["dns" "reach" "gateway"];
      }
    ];
    deployment = {
      ansible.enable = true;
      deployRs.enable = true;
      sops.recipient = "age1showcaseshowcaseshowcaseshowcaseshowcaseshowcaseshowcasewb";
    };
  };

  boot.loader.grub.device = "/dev/sda";
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
  };
  system.stateVersion = "26.05";
}
