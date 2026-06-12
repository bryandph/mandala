# Fictional overlay-mesh table. Keys are the controller-side member
# identifiers (in a real fleet: terraform state addresses). memberId is
# controller-assigned and never derivable in-config; dnsName is authored
# where a mesh DNS record should exist.
{
  members = {
    web-mesh = {
      memberId = "feedface01";
      ip = "10.144.0.10";
      name = "web";
      dnsName = "web.mesh";
    };
    op-phone = {
      memberId = "feedface02";
      ip = "10.144.0.20";
    };
  };
}
