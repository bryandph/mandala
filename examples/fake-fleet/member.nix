# A fake member exercising the schema: cross-built NixOS box whose DNS
# name and deploy reach live on a mesh overlay while the default route
# is a different NIC — the case a single "primary" flag can't model.
{
  name = "example-node";
  domain = "example.test";
  category = "server";
  role = "cache";
  platform = "nixos";
  tags = ["edge" "fake"];
  build = {
    system = "armv7l-linux";
    buildPlatform = "aarch64-linux";
  };
  networks = [
    {
      vlan = "mgmt";
      address = "10.99.0.5";
      interface = "eth0";
      roles = ["gateway"];
      assignment = "reservation";
    }
    {
      vlan = "storage";
      address = "10.77.0.5";
      roles = ["dns" "reach"];
    }
  ];
  zerotier = {
    memberId = "0123456789";
    address = "10.99.42.5";
  };
  deployment.ansible.groups = ["fake_extra_group"];
}
