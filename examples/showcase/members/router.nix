# Facts-only member: appears in the taxonomy and data projections,
# nothing pushes to it (management surfaces stay off).
{
  name = "router";
  domain = "fleet.example";
  platform = "opnsense";
  category = "gateway";
  networks = [
    {
      vlan = "lan";
      address = "10.10.0.1";
      roles = ["dns" "gateway" "management"];
    }
  ];
}
