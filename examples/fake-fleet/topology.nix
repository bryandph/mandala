# Fake topology used by `checks.<system>.fake-fleet`. No value here
# belongs to a real network.
{
  vlans = {
    mgmt = {
      id = 2;
      subnet = "10.0.2.0/24";
      gateway = "10.0.2.1";
      dns = ["10.0.2.2"];
      domain = "mgmt.example";
      description = "Example management network";
      ula = "fd00:dead:beef:2::/64";
      ulaGateway = "fd00:dead:beef:2::1";
      slaId = 2;
    };
    # id-only: on the switch trunk, no routed subnet authored.
    storage = {
      id = 99;
      description = "Example id-only network";
    };
  };
}
