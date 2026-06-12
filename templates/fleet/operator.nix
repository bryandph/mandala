# Operator identity — replace every value with your own. The gpg
# fingerprint anchors generated sops creation rules (the operator can
# always decrypt); sshPublicKeys are the keys your members authorize.
# Only the full 40-hex fingerprint is authored — short ids derive.
{
  name = "operator";
  fullname = "Example Operator";
  email = "operator@example.com";
  gpg = {
    fingerprint = "0123456789ABCDEF0123456789ABCDEF01234567";
    publicKeyUrl = "https://example.com/keys/operator.asc";
  };
  sshPublicKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderPlaceholderPlaceholderPlacehol operator@example"
  ];
}
