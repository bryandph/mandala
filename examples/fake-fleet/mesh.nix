# Fake overlay table: one mesh-leaf with a derived DNS name, one
# device-only member without.
{
  members = {
    example-node-mesh = {
      memberId = "0123456789";
      ip = "10.99.42.5";
      name = "example-node.example.test";
      dnsName = "example-node.mesh";
    };
    example-phone = {
      memberId = "abcdef0123";
      ip = "10.99.42.125";
    };
  };
}
