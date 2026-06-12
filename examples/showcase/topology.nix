# Fictional network topology: one addressed VLAN, one id-only.
{
  vlans = {
    lan = {
      id = 10;
      subnet = "10.10.0.0/24";
    };
    mgmt.id = 99;
  };
}
