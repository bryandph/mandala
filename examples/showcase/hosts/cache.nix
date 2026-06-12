# Second NixOS member, different architecture: the taxonomy carries the
# arch facet (aarch64 group) and the DHCP-reservation addressing form.
{
  host = {
    name = "cache";
    domain = "fleet.example";
    category = "server";
    role = "cache";
    tags = ["showcase"];
    build.system = "aarch64-linux";
    networks = [
      {
        vlan = "lan";
        address = "10.10.0.11";
        interface = "end0";
        mac = "dc:a6:32:00:00:11";
        roles = ["dns" "reach" "gateway"];
        assignment = "reservation";
      }
    ];
    deployment = {
      ansible.enable = true;
      deployRs.enable = true;
      sops.recipient = "age1showcaseshowcaseshowcaseshowcaseshowcaseshowcaseshowcasech";
    };
  };

  boot.loader.grub.device = "/dev/vda";
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
  };
  system.stateVersion = "26.05";
}
